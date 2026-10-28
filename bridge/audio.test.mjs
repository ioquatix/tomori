import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { test } from "node:test";
import { AudioMixer, BYTES_PER_TICK, wavHeader } from "./audio.mjs";

test("live audio WAV header identifies 16 kHz mono PCM", () => {
  const h = wavHeader();
  assert.equal(h.toString("ascii", 0, 4), "RIFF");
  assert.equal(h.readUInt16LE(22), 1);
  assert.equal(h.readUInt32LE(24), 16000);
  assert.equal(h.readUInt16LE(34), 16);
});

test("mixes independent inputs, clamps peaks, sends silence and bounds backlog", () => {
  const m = new AudioMixer();
  const chunks = [];
  const listener = new EventEmitter();
  listener.writeHead = () => {};
  listener.write = data => { chunks.push(Buffer.from(data)); return true; };
  m.subscribe(listener);
  const mic = Buffer.alloc(BYTES_PER_TICK); const agent = Buffer.alloc(BYTES_PER_TICK);
  for (let i = 0; i < BYTES_PER_TICK; i += 2) { mic.writeInt16LE(20000, i); agent.writeInt16LE(20000, i); }
  m.push("mic", mic); m.push("agent", agent); m.tick();
  assert.equal(chunks[1].readInt16LE(0), 32767);
  m.tick(); assert.equal(chunks[2].readInt16LE(0), 0);
  for (let i = 0; i < 10; i++) m.push("mic", mic);
  assert.ok(m.sources.mic.length <= BYTES_PER_TICK * 3);
  assert.throws(() => m.push("mic", Buffer.alloc(3)));
  listener.emit("close"); m.tick(); assert.equal(m.sources.mic.length, 0);
  clearInterval(m.timer);
});

test("microphone and agent outputs remain isolated", () => {
  const m = new AudioMixer();
  function listener(source) {
    const chunks = [];
    const out = new EventEmitter();
    out.writeHead = () => {};
    out.write = data => { chunks.push(Buffer.from(data)); return true; };
    m.subscribe(out, source);
    return { out, chunks };
  }
  const mic = listener("mic"), agent = listener("agent");
  const data = Buffer.alloc(BYTES_PER_TICK);
  data.writeInt16LE(1000, 0);
  m.push("mic", data); m.tick();
  assert.equal(mic.chunks[1].readInt16LE(0), 1000);
  assert.equal(agent.chunks[1].readInt16LE(0), 0);
  mic.out.emit("close"); agent.out.emit("close"); clearInterval(m.timer);
});
