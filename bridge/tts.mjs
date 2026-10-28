import { Worker } from "node:worker_threads";
import { resolve } from "node:path";
import { existsSync } from "node:fs";
import { InputError } from "./core.mjs";

export class Tts {
  constructor({ modelDir = resolve(".tools/tts/kokoro-multi-lang-v1_0"), voice = 37 } = {}) {
    this.modelDir = modelDir;
    this.voice = voice;
    this.worker = null;
    this.ready = null;
    this.pending = new Map();
    this.next = 0;
  }

  start() {
    if (this.ready) return this.ready;
    if (!["model.onnx", "voices.bin", "tokens.txt", "lexicon-us-en.txt", "espeak-ng-data"].every(file => existsSync(resolve(this.modelDir, file)))) {
      return Promise.reject(new Error("Kokoro model missing; run bash scripts/fetch-tts-model.sh on the PC"));
    }
    if (!Number.isInteger(this.voice) || this.voice < 0 || this.voice > 53) return Promise.reject(new Error("Kokoro voice must be an integer from 0 to 53"));
    const worker = new Worker(new URL("./tts-worker.mjs", import.meta.url), { workerData: { modelDir: this.modelDir, voice: this.voice } });
    this.worker = worker;
    this.ready = new Promise((resolveReady, rejectReady) => {
      worker.on("message", message => {
        if (message.ready) {
          if (this.voice >= message.voices) rejectReady(new Error(`Voice ${this.voice} is unavailable in this model`));
          else resolveReady(message);
          return;
        }
        if (message.id === undefined && message.error) { rejectReady(new Error(message.error)); return; }
        const request = this.pending.get(message.id);
        if (!request) return;
        this.pending.delete(message.id);
        if (message.error) request.reject(new Error(message.error));
        else request.resolve({ sampleRate: message.sampleRate, samples: Buffer.from(message.samples) });
      });
      worker.on("error", error => { rejectReady(error); this.fail(error); });
      worker.on("exit", code => {
        const error = new Error(`Kokoro worker exited (${code})`);
        rejectReady(error);
        this.fail(error);
        this.worker = null;
        this.ready = null;
      });
    });
    return this.ready;
  }

  fail(error) {
    for (const request of this.pending.values()) request.reject(error);
    this.pending.clear();
  }

  async synthesize(text) {
    if (typeof text !== "string" || !text.trim() || text.length > 500) throw new InputError("Speech requires 1–500 characters.");
    await this.start();
    if (this.pending.size >= 4) throw new Error("Speech generator is busy. Try again shortly.");
    const id = ++this.next;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({ id, text: text.trim() });
    });
  }
}
