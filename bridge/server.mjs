import { createServer } from "node:http";
import { OpenCode } from "@opencode/client";
import { Service } from "@opencode/client/service";
import { Bridge, InputError } from "./core.mjs";
import { Delegation, DelegationError } from "./delegation.mjs";
import { fileURLToPath } from "node:url";
import { Tts } from "./tts.mjs";
import { AudioMixer } from "./audio.mjs";
import { Asr } from "./asr.mjs";
import { validControllerPoses, validHands } from "./controller-poses.mjs";
import { validInteraction } from "./interaction.mjs";
import { bodyOf, SPECTATOR_JSON_LIMIT } from "./json-body.mjs";

const port = Number(process.env.BASE_SWARM_BRIDGE_PORT || 8765);
const endpoint = await Service.ensure();
const client = OpenCode.make({ baseUrl: endpoint.url, headers: Service.headers(endpoint) });
const info = await client.server.info();
const bridge = new Bridge(client, { delegation: new Delegation(client, {
  storePath: fileURLToPath(new URL("../.space/presentations.json", import.meta.url)),
  visibilityPath: fileURLToPath(new URL("../.space/worker-visibility.json", import.meta.url)),
}) });
const tts = new Tts({ voice: Number(process.env.BASE_SWARM_TTS_VOICE || 37) });
let spectatorFrame = null;
let spectatorFrameTime = 0;
const audience = new AudioMixer();
const asr = new Asr();

createServer(async (request, response) => {
  const reply = (status, data) => {
    response.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
    response.end(JSON.stringify(data));
  };
  try {
    // Bind only loopback and do not offer browser cross-origin access. The
    // headset reaches this port via adb reverse, not a public LAN listener.
    if (![ `127.0.0.1:${port}`, `localhost:${port}` ].includes(request.headers.host) || request.headers.origin) {
      return reply(403, { error: "Loopback native clients only." });
    }
    const path = new URL(request.url, `http://127.0.0.1:${port}`).pathname;
    if (request.method === "GET" && path === "/health") return reply(200, { version: info.version });
    if (request.method === "POST" && path === "/transcription/start") {
      const body = await bodyOf(request);
      return reply(200, await asr.begin(body.stream));
    }
    const transcribe = /^\/transcription\/(vr_[0-9a-f_]{1,80})\/(audio|finish|cancel)$/.exec(path);
    if (request.method === "POST" && transcribe) {
      const [, stream, operation] = transcribe;
      if (operation === "audio") {
        if (!request.headers["content-type"]?.startsWith("application/octet-stream")) throw new InputError("PCM audio required.");
        const chunks = []; let size = 0;
        for await (const chunk of request) { size += chunk.length; if (size > 64000) throw new InputError("Audio chunk too large."); chunks.push(chunk); }
        const sequence = Number(request.headers["x-audio-sequence"]);
        if (!Number.isSafeInteger(sequence) || sequence < 0) throw new InputError("Invalid audio sequence");
        return reply(200, await asr.audio(stream, sequence, Buffer.concat(chunks)));
      }
      const body = await bodyOf(request);
      if (operation === "finish") return reply(200, await asr.finish(stream, body.sequence));
      return reply(200, await asr.cancel(stream));
    }
    if (request.method === "GET" && path === "/spectator/audio.wav") return audience.subscribe(response);
    if (request.method === "GET" && path === "/spectator/microphone.wav") return audience.subscribe(response, "mic");
    if (request.method === "GET" && path === "/spectator/agent.wav") return audience.subscribe(response, "agent");
    if (request.method === "POST" && (path === "/spectator/audio/mic" || path === "/spectator/audio/agent")) {
      if (!request.headers["content-type"]?.startsWith("application/octet-stream")) throw new InputError("PCM audio required.");
      const chunks = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; if (size > 9600) throw new InputError("Audio packet too large."); chunks.push(chunk); }
      try { audience.push(path.endsWith("/mic") ? "mic" : "agent", Buffer.concat(chunks)); }
      catch { throw new InputError("Invalid PCM audio packet."); }
      return reply(200, { ok: true });
    }
    if (request.method === "GET" && path === "/spectator/frame") {
      if (!spectatorFrame || Date.now() - spectatorFrameTime > 2000) {
        response.writeHead(204, { "cache-control": "no-store" });
        return response.end();
      }
      return reply(200, spectatorFrame);
    }
    if (request.method === "POST" && path === "/spectator/frame") {
      const frame = await bodyOf(request, SPECTATOR_JSON_LIMIT);
      const numbers = (arr, n) => Array.isArray(arr) && arr.length === n && arr.every(Number.isFinite);
      if (!Number.isSafeInteger(frame.sequence) || frame.sequence < 0 ||
          !numbers(frame.projection, 16) || !numbers(frame.scene_to_space, 16) || !numbers(frame.head, 16) ||
          !Array.isArray(frame.agents) || frame.agents.length !== 128 || !frame.agents.every(p => numbers(p, 3)) ||
          !(frame.controller === null || numbers(frame.controller, 3)) || typeof frame.attracting !== "boolean" ||
          !validControllerPoses(frame.controllers) ||
          !validHands(frame.hands) ||
          !validInteraction(frame.interaction) ||
          !Number.isFinite(frame.view_aspect) || frame.view_aspect < 0.25 || frame.view_aspect > 4 ||
          typeof frame.caption !== "string" || frame.caption.length > 8192 ||
          !(frame.recording_ms === null || (Number.isSafeInteger(frame.recording_ms) && frame.recording_ms >= 0))) {
        throw new InputError("Invalid spectator frame.");
      }
      spectatorFrame = frame;
      spectatorFrameTime = Date.now();
      return reply(200, { ok: true });
    }
    if (request.method === "GET" && path === "/sessions") return reply(200, await bridge.sessions());
    if (request.method === "POST" && path === "/tts") {
      const body = await bodyOf(request);
      const audio = await tts.synthesize(body.text);
      response.writeHead(200, { "content-type": "application/octet-stream", "cache-control": "no-store",
        "x-sample-rate": String(audio.sampleRate), "content-length": audio.samples.length });
      return response.end(audio.samples);
    }
    const workers = /^\/session\/(ses[a-zA-Z0-9_-]{1,125})\/workers\/(acknowledged|(ses[a-zA-Z0-9_-]{1,125})\/acknowledge)$/.exec(path);
    if (workers) {
      const [, root, operation, worker] = workers;
      if (request.method === "GET" && operation === "acknowledged") return reply(200, await bridge.delegation.acknowledged(root));
      if (request.method === "POST" && worker) return reply(200, await bridge.delegation.acknowledge(root, worker));
      if (request.method === "DELETE" && worker) return reply(200, await bridge.delegation.restore(root, worker));
      return reply(404, { error: "Unknown bridge route." });
    }
    const match = /^\/session\/(ses[a-zA-Z0-9_-]{1,125})(?:\/(message|permission|form|delegation|presentations)(?:\/([a-zA-Z0-9_-]{1,128}))?)?$/.exec(path);
    if (!match) return reply(404, { error: "Unknown bridge route." });
    const [, sessionID, operation, id] = match;
    if (request.method === "GET" && !operation) return reply(200, await bridge.snapshot(sessionID));
    if (request.method === "GET" && operation === "delegation" && !id) return reply(200, await bridge.delegation.get(sessionID));
    if (request.method === "DELETE" && operation === "presentations" && id) return reply(200, await bridge.delegation.remove(sessionID, id));
    if (request.method === "POST") {
      const body = await bodyOf(request);
      if (operation === "message" && !id) return reply(200, await bridge.send(sessionID, body));
      if (operation === "permission" && id) return reply(200, await bridge.permission(sessionID, id, body));
      if (operation === "form" && id) return reply(200, await bridge.answer(sessionID, id, body));
      if (operation === "presentations" && !id) return reply(200, await bridge.delegation.publish(sessionID, body));
    }
    reply(404, { error: "Unknown bridge route." });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    reply(error instanceof DelegationError ? error.status : error instanceof InputError ? 400 : 502, { error: detail });
  }
}).listen(port, "127.0.0.1", () => {
  console.log(`Quest bridge on http://127.0.0.1:${port}; connected to OpenCode ${info.version}.`);
  console.log(`Forward USB with: adb reverse tcp:8765 tcp:${port}`);
});
