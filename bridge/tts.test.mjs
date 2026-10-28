import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import { Tts } from "./tts.mjs";

const root = resolve("..", ".tools/tts/kokoro-multi-lang-v1_0");

test("validates requests without loading a model", async () => {
  const tts = new Tts({ modelDir: "/missing-kokoro" });
  await assert.rejects(tts.synthesize(""), /Speech requires/);
  await assert.rejects(tts.synthesize("a".repeat(501)), /Speech requires/);
  await assert.rejects(tts.synthesize("Hello"), /model missing/);
});

test("voice selection validates the new model's speaker range", async () => {
  if (!existsSync(resolve(root, "model.onnx"))) return;
  const tts = new Tts({ modelDir: root, voice: 54 });
  await assert.rejects(tts.synthesize("Hello"), /0 to 53/);
  assert.equal(tts.worker, null);
});

test("Japanese-trained voice generates real English speech off the main thread", { skip: !existsSync(resolve(root, "model.onnx")) }, async () => {
  const tts = new Tts({ modelDir: root });
  try {
    const audio = await tts.synthesize("Hello, the headset voice is ready.");
    assert.equal(audio.sampleRate, 24000);
    const samples = new Float32Array(audio.samples.buffer, audio.samples.byteOffset, audio.samples.byteLength / 4);
    assert.ok(samples.length > 24000);
    assert.ok(samples.some(x => Math.abs(x) > 0.01));
    assert.ok(samples.every(x => Number.isFinite(x) && Math.abs(x) <= 1));
  } finally {
    await tts.worker?.terminate();
  }
});
