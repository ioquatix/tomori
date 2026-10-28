import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import { Asr } from "./asr.mjs";

const root = resolve("..");
const ready = existsSync(resolve(root, ".tools/asr/faster-whisper-small.en/model.bin")) && existsSync(resolve(root, "target/downloads/jfk.wav"));

test("enforces utterance identity and single active recording before loading worker", async () => {
  const asr = new Asr({ root });
  try {
    await assert.rejects(asr.begin("bad"), /Invalid transcription ID/);
    await assert.rejects(asr.audio("vr_test", 0, Buffer.alloc(800)), /Missing or out-of-order/);
    assert.equal(asr.worker, null);
  } finally { clearInterval(asr.cleanup); }
});

test("real streaming Whisper produces partial text and finalizes", { skip: !ready, timeout: 120000 }, async () => {
  const asr = new Asr({ root });
  try {
    const wav = readFileSync(resolve(root, "target/downloads/jfk.wav"));
    assert.equal(wav.toString("ascii", 0, 4), "RIFF");
    // Locate RIFF data rather than assuming a fixed-length header.
    let offset = 12, pcm;
    while (offset + 8 <= wav.length) {
      const length = wav.readUInt32LE(offset + 4), name = wav.toString("ascii", offset, offset + 4);
      if (name === "data") { pcm = wav.subarray(offset + 8, offset + 8 + length); break; }
      offset += 8 + length + (length & 1);
    }
    assert.ok(pcm);
    await asr.begin("vr_123");
    await assert.rejects(asr.begin("vr_999"), /recording is active/);
    let sequence = 0, partial = "";
    for (let offset = 0; offset < pcm.length; offset += 8000) {
      const response = await asr.audio("vr_123", sequence++, pcm.subarray(offset, offset + 8000));
      if (response.text) partial = response.text;
    }
    assert.ok(partial.toLowerCase().includes("country"), `Partial: ${partial}`);
    const final = await asr.finish("vr_123", sequence);
    assert.ok(final.text.toLowerCase().includes("country"), `Final: ${final.text}`);
    assert.equal(asr.active, null);
    await asr.begin("vr_456");
    await assert.rejects(asr.audio("vr_456", 1, Buffer.alloc(800)), /out-of-order/);
    await asr.cancel("vr_456");

    // More than 60 seconds, split into ordered one-second packets. Earlier
    // windows must survive even as their PCM is released from memory.
    await asr.begin("vr_789");
    let longSequence = 0;
    for (let repetition = 0; repetition < 7; repetition++) {
      for (let offset = 0; offset < pcm.length; offset += 32000) {
        await asr.audio("vr_789", longSequence++, pcm.subarray(offset, offset + 32000));
      }
    }
    assert.ok(pcm.length * 7 > 16000 * 2 * 60);
    const longFinal = await asr.finish("vr_789", longSequence);
    const countries = (longFinal.text.toLowerCase().match(/country/g) || []).length;
    assert.ok(countries >= 10 && countries <= 14,
      `Long transcript lost repeated speech: ${longFinal.text}`);
  } finally {
    clearInterval(asr.cleanup);
    if (asr.worker) { const worker = asr.worker; worker.stdin.end(); await new Promise(resolve => worker.once("exit", resolve)); }
  }
});
