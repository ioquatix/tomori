//! Publish the bridge's live WAV feeds as local PipeWire audio sources.
//! This helper is separate from the bridge so either can restart independently.
import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { get } from "node:http";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { BYTES_PER_TICK, SAMPLE_RATE } from "./audio.mjs";

const TICK_MS = 50;
const MAX_PENDING = BYTES_PER_TICK * 3;
const HEADER_BYTES = 44;
const SOURCES = [
  { path: "/spectator/microphone.wav", node: "base-swarm-quest-microphone", sink: "base-swarm-quest-microphone-sink", description: "Base-Swarm-Quest-Microphone" },
  { path: "/spectator/agent.wav", node: "base-swarm-assistant-voice", sink: "base-swarm-assistant-voice-sink", description: "Base-Swarm-Assistant-Voice" },
];

// The bridge produces the fixed 44-byte PCM WAV header in audio.mjs. Incoming
// HTTP chunks may split either the header or a 16-bit sample.
export class PcmBuffer {
  constructor() { this.reset(); }
  reset() { this.header = Buffer.alloc(0); this.ready = false; this.pending = Buffer.alloc(0); this.partial = Buffer.alloc(0); }
  push(chunk) {
    if (!this.ready) {
      const need = HEADER_BYTES - this.header.length;
      const taken = Math.min(need, chunk.length);
      this.header = Buffer.concat([this.header, chunk.subarray(0, taken)]);
      chunk = chunk.subarray(taken);
      if (this.header.length < HEADER_BYTES) return;
      const h = this.header;
      if (h.toString("ascii", 0, 4) !== "RIFF" || h.toString("ascii", 8, 12) !== "WAVE" ||
          h.toString("ascii", 12, 16) !== "fmt " || h.toString("ascii", 36, 40) !== "data" ||
          h.readUInt32LE(16) !== 16 || h.readUInt16LE(20) !== 1 || h.readUInt16LE(22) !== 1 ||
          h.readUInt32LE(24) !== SAMPLE_RATE || h.readUInt16LE(34) !== 16) {
        throw new Error("Bridge audio is not 16 kHz mono S16LE PCM WAV");
      }
      this.ready = true;
      this.header = Buffer.alloc(0);
    }
    if (!chunk.length) return;
    const bytes = this.partial.length ? Buffer.concat([this.partial, chunk]) : chunk;
    const length = bytes.length - bytes.length % 2;
    this.partial = Buffer.from(bytes.subarray(length));
    const combined = Buffer.concat([this.pending, bytes.subarray(0, length)]);
    this.pending = Buffer.from(combined.subarray(Math.max(0, combined.length - MAX_PENDING)));
  }
  next() {
    const result = Buffer.alloc(BYTES_PER_TICK);
    const length = Math.min(this.pending.length, result.length);
    this.pending.copy(result, 0, 0, length);
    this.pending = this.pending.subarray(length);
    return result;
  }
}

export function pipelineArgs({ sink }) {
  return [
    "-q", "fdsrc", "fd=0", `blocksize=${BYTES_PER_TICK}`, "!",
    "rawaudioparse", "format=pcm", "pcm-format=s16le", `sample-rate=${SAMPLE_RATE}`, "num-channels=1", "!",
    "queue", "leaky=downstream", "max-size-buffers=3", "max-size-bytes=0", "max-size-time=0", "!",
    "pulsesink", `device=${sink}`, "sync=false", "buffer-time=100000", "latency-time=10000",
  ];
}

export class AudioFeed {
  constructor({ source, port, onFailure, spawnProcess = spawn, request = get, log = console }) {
    this.source = source;
    this.port = port;
    this.onFailure = onFailure;
    this.spawnProcess = spawnProcess;
    this.request = request;
    this.log = log;
    this.pcm = new PcmBuffer();
    this.stopped = false;
    this.retry = null;
    this.req = null;
    this.res = null;
    this.child = null;
    this.timer = null;
    this.generation = 0;
    this.waitingLogged = false;
    this.connected = false;
  }
  start() {
    this.child = this.spawnProcess("gst-launch-1.0", pipelineArgs(this.source), { stdio: ["pipe", "ignore", "inherit"] });
    this.child.once("error", error => this.fail(`starting GStreamer: ${error.message}`));
    this.child.once("exit", (code, signal) => this.fail(`GStreamer exited (${signal || code})`));
    this.child.stdin.on("error", error => this.fail(`GStreamer input: ${error.message}`));
    this.timer = setInterval(() => this.tick(), TICK_MS);
    this.connect();
  }
  fail(message) {
    if (!this.stopped) this.onFailure(`${this.source.description}: ${message}`);
  }
  tick() {
    const frame = this.pcm.next();
    const input = this.child?.stdin;
    // Never queue unbounded data in Node when GStreamer is blocked. The
    // downstream leaky queue also drops old audio if no consumer is attached.
    if (input && !input.destroyed && input.writable && input.writableLength === 0) input.write(frame);
  }
  connect() {
    if (this.stopped) return;
    this.pcm.reset();
    const generation = ++this.generation;
    const req = this.request({ hostname: "127.0.0.1", port: this.port, path: this.source.path, timeout: 5000 }, res => {
      if (this.stopped || generation !== this.generation) { res.destroy(); return; }
      this.res = res;
      res.once("error", () => this.disconnected(generation));
      res.once("end", () => this.disconnected(generation));
      res.once("close", () => this.disconnected(generation));
      if (res.statusCode !== 200) {
        this.log.warn(`${this.source.description}: bridge returned HTTP ${res.statusCode}`);
        res.resume();
        return;
      }
      res.on("data", chunk => {
        try {
          this.pcm.push(chunk);
          if (this.pcm.ready && !this.connected) {
            this.connected = true;
            this.waitingLogged = false;
            this.log.log(`${this.source.description}: connected to bridge`);
          }
        } catch (error) { this.log.warn(`${this.source.description}: ${error.message}`); res.destroy(); }
      });
    });
    this.req = req;
    req.once("timeout", () => req.destroy(new Error("bridge timeout")));
    req.once("error", () => this.disconnected(generation));
  }
  disconnected(generation) {
    if (this.stopped || generation !== this.generation) return;
    this.generation++;
    this.pcm.reset(); // Drop any old audio and emit silence while reconnecting.
    this.connected = false;
    if (!this.waitingLogged) {
      this.log.warn(`${this.source.description}: bridge unavailable; outputting silence while reconnecting`);
      this.waitingLogged = true;
    }
    this.res = null;
    this.req = null;
    if (!this.retry) {
      this.retry = setTimeout(() => { this.retry = null; this.connect(); }, 1000);
    }
  }
  stop() {
    if (this.stopped) return;
    this.stopped = true;
    clearInterval(this.timer);
    clearTimeout(this.retry);
    this.pcm.reset();
    this.res?.destroy();
    this.req?.destroy();
    this.child?.stdin.destroy();
    if (this.child && this.child.exitCode === null && this.child.signalCode === null) this.child.kill("SIGTERM");
  }
}

const execFileAsync = promisify(execFile);
async function pactl(...args) {
  const { stdout } = await execFileAsync("pactl", args, { timeout: 5000, maxBuffer: 4 * 1024 * 1024 });
  return stdout.trim();
}

export function ownedDevice(devices, name, moduleId) {
  return devices.some(device => device.name === name && Number(device.owner_module) === moduleId);
}

export function ownedModule(list, module) {
  return list.split("\n").some(line => {
    const [id, type, args = ""] = line.split("\t");
    const fields = args.split(/\s+/);
    return Number(id) === module.id && type === module.type &&
      fields.includes(`${module.key}=${module.node}`) && fields.includes(`base.swarm.instance=${module.instance}`);
  });
}

export async function run({ port = Number(process.env.BASE_SWARM_BRIDGE_PORT || 8765) } = {}) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid BASE_SWARM_BRIDGE_PORT");
  const feeds = [];
  const modules = [];
  const loading = [];
  const instance = randomBytes(12).toString("hex");
  let stopping = false;
  let stopPromise;
  function stop(failure) {
    if (stopping) return stopPromise;
    stopping = true;
    if (failure) { console.error(`PipeWire audio stopped: ${failure}`); process.exitCode = 1; }
    for (const feed of feeds) feed.stop();
    stopPromise = (async () => {
      // Wait for any in-flight module creation before cleaning up its ID.
      await Promise.allSettled(loading);
      let current;
      try { current = await pactl("list", "short", "modules"); }
      catch (error) {
        if (modules.length) { console.error(`Cannot verify audio modules for cleanup: ${error.message}`); process.exitCode = 1; }
        return;
      }
      for (const module of modules.reverse()) {
        // A restarted Pulse server can reuse module IDs; do not unload a
        // different module if ours is no longer present. The random tag also
        // distinguishes this helper from another instance using the same name.
        if (!ownedModule(current, module)) continue;
        try { await pactl("unload-module", String(module.id)); }
        catch (error) { console.error(`Could not remove ${module.description} (module ${module.id}): ${error.message}`); process.exitCode = 1; }
      }
    })();
    return stopPromise;
  }
  process.once("SIGINT", () => { void stop(); });
  process.once("SIGTERM", () => { void stop(); });
  process.once("SIGHUP", () => { void stop(); });
  try {
    const existingSinks = JSON.parse(await pactl("-f", "json", "list", "sinks"));
    const existingSources = JSON.parse(await pactl("-f", "json", "list", "sources"));
    if (stopping) return await stopPromise;
    for (const source of SOURCES) {
      if (existingSinks.some(sink => sink.name === source.sink) || existingSources.some(input => input.name === source.node)) {
        throw new Error(`${source.description} already exists; stop the other helper before starting another`);
      }
    }
    for (const source of SOURCES) {
      const create = pactl("load-module", "module-null-sink", `sink_name=${source.sink}`,
        `rate=${SAMPLE_RATE}`, "channels=1",
        `sink_properties=device.description=${source.description}-Internal node.virtual=true priority.session=1 base.swarm.instance=${instance}`)
        .then(output => {
          const id = Number(output);
          if (!Number.isSafeInteger(id) || id < 0 || !/^\d+$/.test(output)) throw new Error(`Invalid PulseAudio module ID: ${output}`);
          modules.push({ id, node: source.sink, key: "sink_name", type: "module-null-sink", instance, description: `${source.description} sink` });
          return id;
        });
      loading.push(create);
      const id = await create;
      if (stopping) return await stopPromise;
      const sinks = JSON.parse(await pactl("-f", "json", "list", "sinks"));
      if (stopping) return await stopPromise;
      if (!ownedDevice(sinks, source.sink, id)) throw new Error(`Created module ${id} does not own ${source.sink}`);
      const createInput = pactl("load-module", "module-remap-source", `master=${source.sink}.monitor`,
        `source_name=${source.node}`, `rate=${SAMPLE_RATE}`, "channels=1", "channel_map=mono", "master_channel_map=mono",
        `source_properties=device.description=${source.description} node.virtual=true priority.session=1 base.swarm.instance=${instance}`)
        .then(output => {
          const inputId = Number(output);
          if (!Number.isSafeInteger(inputId) || inputId < 0 || !/^\d+$/.test(output)) throw new Error(`Invalid PulseAudio module ID: ${output}`);
          modules.push({ id: inputId, node: source.node, key: "source_name", type: "module-remap-source", instance, description: source.description });
          return inputId;
        });
      loading.push(createInput);
      const inputId = await createInput;
      if (stopping) return await stopPromise;
      const inputs = JSON.parse(await pactl("-f", "json", "list", "sources"));
      if (stopping) return await stopPromise;
      if (!ownedDevice(inputs, source.node, inputId)) throw new Error(`Created module ${inputId} does not own ${source.node}`);
      const feed = new AudioFeed({ source, port, onFailure: stop });
      feeds.push(feed);
      feed.start();
    }
    console.log("PipeWire audio sources: Base-Swarm-Quest-Microphone and Base-Swarm-Assistant-Voice; Ctrl-C to stop.");
  } catch (error) {
    await stop(error.message);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  run().catch(error => { console.error(`PipeWire audio stopped: ${error.message}`); process.exitCode = 1; });
}
