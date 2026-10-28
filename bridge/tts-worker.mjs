import { parentPort, workerData } from "node:worker_threads";
import { createRequire } from "node:module";
import { join } from "node:path";

const require = createRequire(import.meta.url);
try {
  const sherpa = require("sherpa-onnx-node");
  const root = workerData.modelDir;
  const tts = new sherpa.OfflineTts({
    model: {
      kokoro: { model: join(root, "model.onnx"), voices: join(root, "voices.bin"),
        tokens: join(root, "tokens.txt"), dataDir: join(root, "espeak-ng-data"),
        lexicon: join(root, "lexicon-us-en.txt") },
      numThreads: 2, provider: "cpu", debug: false,
    },
    maxNumSentences: 1,
  });
  parentPort.postMessage({ ready: true, sampleRate: tts.sampleRate, voices: tts.numSpeakers });
  parentPort.on("message", ({ id, text }) => {
    try {
      const audio = tts.generate({ text, sid: workerData.voice, speed: 1.0 });
      if (!audio.samples.length || audio.sampleRate !== 24000) throw new Error("Kokoro returned no usable 24 kHz audio");
      // Copy into an owned ArrayBuffer before transferring it across threads.
      const samples = Float32Array.from(audio.samples);
      parentPort.postMessage({ id, sampleRate: audio.sampleRate, samples: samples.buffer }, [samples.buffer]);
    } catch (error) {
      parentPort.postMessage({ id, error: error.message });
    }
  });
} catch (error) {
  parentPort.postMessage({ error: `Kokoro unavailable: ${error.message}` });
}
