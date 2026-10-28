import { spawn } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { createInterface } from "node:readline";
import { InputError } from "./core.mjs";

const STREAM_ID = /^vr_[0-9a-f_]{1,80}$/;

// The worker handles one recording at a time; requests are serialized and a
// broken/missing chunk fails the utterance rather than silently dropping words.
export class Asr {
  constructor({ root = resolve("."), launch = spawn } = {}) {
    this.root = root; this.launch = launch; this.worker = null; this.ready = null;
    this.pending = new Map(); this.next = 0; this.active = null;
    this.cleanup = setInterval(() => {
      if (this.active && !this.active.busy && Date.now() - this.active.last > 20000) {
        const id = this.active.id;
        this.active = null;
        void this.call("cancel", id).catch(() => {});
      }
    }, 5000);
    this.cleanup.unref();
  }

  startWorker() {
    if (this.ready) return this.ready;
    const venv = join(this.root, ".tools/asr-venv");
    const python = join(venv, "bin/python");
    const model = join(this.root, ".tools/asr/faster-whisper-small.en");
    if (!existsSync(python) || !existsSync(join(model, "model.bin"))) {
      return Promise.reject(new Error("PC transcription unavailable; run bash scripts/setup-pc-transcription.sh"));
    }
    const lib = join(venv, "lib");
    const py = readdirSync(lib).find(name => /^python3\./.test(name));
    if (!py) return Promise.reject(new Error("PC transcription environment is incomplete"));
    const site = join(lib, py, "site-packages/nvidia");
    const env = { ...process.env, BASE_SWARM_ASR_MODEL: model,
      LD_LIBRARY_PATH: [join(site, "cublas/lib"), join(site, "cudnn/lib"), process.env.LD_LIBRARY_PATH || ""].join(":") };
    const worker = this.launch(python, [join(this.root, "bridge/asr_worker.py")], { cwd: this.root, env, stdio: ["pipe", "pipe", "pipe"] });
    this.worker = worker;
    this.ready = new Promise((resolveReady, rejectReady) => {
      const timer = setTimeout(() => { rejectReady(new Error("PC transcription startup timed out")); worker.kill(); }, 60000);
      createInterface({ input: worker.stdout }).on("line", line => {
        let response;
        try { response = JSON.parse(line); } catch { return; }
        if (response.ready) { clearTimeout(timer); console.log(`PC transcription ready: Whisper ${response.model} on ${response.device}.`); resolveReady(response); return; }
        const pending = this.pending.get(response.id);
        if (!pending) return;
        this.pending.delete(response.id); clearTimeout(pending.timer);
        if (response.error) pending.reject(new Error(response.error)); else pending.resolve(response);
      });
      worker.stderr.on("data", data => process.stderr.write(data));
      worker.on("error", error => { clearTimeout(timer); rejectReady(error); });
      worker.on("exit", code => {
        clearTimeout(timer);
        const error = new Error(`PC transcription worker exited (${code})`);
        rejectReady(error);
        for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
        this.pending.clear(); this.active = null; this.worker = null; this.ready = null;
      });
    });
    return this.ready;
  }

  async call(op, stream, audio) {
    await this.startWorker();
    const id = ++this.next;
    return new Promise((resolveCall, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error("PC transcription timed out; recording cancelled")); this.worker?.kill(); }, 15000);
      this.pending.set(id, { resolve: resolveCall, reject, timer });
      this.worker.stdin.write(JSON.stringify({ id, op, stream, ...(audio ? { audio: audio.toString("base64") } : {}) }) + "\n");
    });
  }

  async begin(stream) {
    if (!STREAM_ID.test(stream || "")) throw new InputError("Invalid transcription ID");
    if (this.active) throw new InputError("Another recording is active; wait or reconnect");
    const active = { id: stream, next: 0, last: Date.now(), busy: true };
    this.active = active;
    try { await this.call("start", stream); active.busy = false; active.last = Date.now(); }
    catch (error) { if (this.active === active) this.active = null; throw error; }
    return { ok: true };
  }

  async audio(stream, sequence, bytes) {
    const active = this.active;
    if (!active || active.id !== stream || active.busy || sequence !== active.next) throw new InputError("Missing or out-of-order audio; recording cancelled");
    if (!bytes.length || bytes.length % 2 || bytes.length > 64000) throw new InputError("Invalid microphone audio");
    active.busy = true; active.last = Date.now();
    try {
      const result = await this.call("audio", stream, bytes);
      active.next++; active.last = Date.now();
      return { text: result.text };
    } finally { active.busy = false; }
  }

  async finish(stream, sequence) {
    const active = this.active;
    if (!active || active.id !== stream || active.busy || sequence !== active.next) throw new InputError("Recording is incomplete; no message sent");
    active.busy = true;
    try { const result = await this.call("finish", stream); return { text: result.text }; }
    finally { if (this.active === active) this.active = null; }
  }

  async cancel(stream) {
    if (this.active?.id !== stream) return { ok: true };
    this.active = null;
    await this.call("cancel", stream);
    return { ok: true };
  }
}
