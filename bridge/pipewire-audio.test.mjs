import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { test } from "node:test";
import { BYTES_PER_TICK, wavHeader } from "./audio.mjs";
import { AudioFeed, PcmBuffer, ownedDevice, ownedModule, pipelineArgs } from "./pipewire-audio.mjs";
const quiet = { log() {}, warn() {} };

test("parses split WAV headers and odd-sized PCM chunks without changing samples", () => {
  const pcm = new PcmBuffer();
  const header = wavHeader();
  pcm.push(header.subarray(0, 7));
  pcm.push(header.subarray(7, 42));
  pcm.push(Buffer.concat([header.subarray(42), Buffer.from([1, 2, 3])]));
  pcm.push(Buffer.from([4, 5, 6]));
  const frame = pcm.next();
  assert.deepEqual(frame.subarray(0, 6), Buffer.from([1, 2, 3, 4, 5, 6]));
  assert.ok(frame.subarray(6).every(byte => byte === 0));
});

test("rejects unexpected format and emits silence after reset", () => {
  const pcm = new PcmBuffer();
  const bad = wavHeader();
  bad.writeUInt32LE(48000, 24);
  assert.throws(() => pcm.push(bad), /16 kHz mono/);
  pcm.reset();
  pcm.push(Buffer.concat([wavHeader(), Buffer.from([7, 8])]));
  pcm.reset();
  assert.ok(pcm.next().every(byte => byte === 0));
});

test("drops old samples under load and limits queued audio", () => {
  const pcm = new PcmBuffer();
  pcm.push(wavHeader());
  for (let i = 1; i <= 8; i++) pcm.push(Buffer.alloc(BYTES_PER_TICK, i));
  assert.equal(pcm.pending.length, 3 * BYTES_PER_TICK);
  assert.equal(pcm.next()[0], 6);
  assert.equal(pcm.next()[0], 7);
  assert.equal(pcm.next()[0], 8);
  assert.equal(pcm.next()[0], 0);
});

test("pipeline targets only its dedicated sink and bounds buffering", () => {
  const args = pipelineArgs({ sink: "test-node", description: "Test-Audio" });
  assert.ok(args.includes("sample-rate=16000"));
  assert.ok(args.includes("num-channels=1"));
  assert.ok(args.includes("leaky=downstream"));
  assert.ok(args.includes("max-size-buffers=3"));
  assert.ok(args.includes("pulsesink"));
  assert.ok(args.includes("device=test-node"));
});

test("only accepts the sink created by the helper's module", () => {
  const sinks = [{ name: "test-node", owner_module: 123 }, { name: "other", owner_module: 456 }];
  assert.equal(ownedDevice(sinks, "test-node", 123), true);
  assert.equal(ownedDevice(sinks, "test-node", 456), false);
  assert.equal(ownedDevice(sinks, "missing", 123), false);
});

test("cleanup rejects reused module IDs and modules from other instances", () => {
  const module = { id: 123, node: "test-node", key: "sink_name", type: "module-null-sink", instance: "a1b2" };
  assert.equal(ownedModule("123\tmodule-null-sink\tsink_name=test-node base.swarm.instance=a1b2\t", module), true);
  assert.equal(ownedModule("123\tmodule-null-sink\tsink_name=test-node base.swarm.instance=other\t", module), false);
  assert.equal(ownedModule("123\tmodule-null-sink\tsink_name=someone-else base.swarm.instance=a1b2\t", module), false);
  assert.equal(ownedModule("123\tmodule-remap-source\tsink_name=test-node base.swarm.instance=a1b2\t", module), false);
});

test("does not queue audio while the child input is backpressured", () => {
  const frames = [];
  const feed = new AudioFeed({ source: { description: "test" }, port: 8765, onFailure: () => assert.fail() });
  const input = { destroyed: false, writable: true, writableLength: 1, write: frame => frames.push(frame) };
  feed.child = { stdin: input };
  feed.pcm.push(Buffer.concat([wavHeader(), Buffer.from([1, 2])]));
  feed.tick();
  assert.equal(frames.length, 0);
  input.writableLength = 0;
  feed.tick();
  assert.equal(frames.length, 1);
  assert.ok(frames[0].every(byte => byte === 0));
});

test("bridge disconnect clears queued audio and schedules only one reconnect", () => {
  const requests = [];
  const request = (_options, _callback) => {
    const req = new EventEmitter();
    req.destroy = () => {};
    requests.push(req);
    return req;
  };
  const feed = new AudioFeed({ source: { path: "/test", description: "test" }, port: 8765, onFailure: () => assert.fail(), request, log: quiet });
  feed.connect();
  feed.pcm.push(Buffer.concat([wavHeader(), Buffer.from([1, 2])]));
  requests[0].emit("error", new Error("offline"));
  assert.ok(feed.pcm.next().every(byte => byte === 0));
  const retry = feed.retry;
  feed.disconnected(1); // A later event from the old request must not replace the retry.
  assert.equal(feed.retry, retry);
  feed.stop();
});

test("non-200 bridge response schedules a reconnect", () => {
  let respond;
  const request = (_options, callback) => {
    respond = callback;
    const req = new EventEmitter();
    req.destroy = () => {};
    return req;
  };
  const feed = new AudioFeed({ source: { path: "/test", description: "test" }, port: 8765, onFailure: () => assert.fail(), request, log: quiet });
  feed.connect();
  const res = new EventEmitter();
  res.statusCode = 503;
  res.resume = () => res.emit("end");
  respond(res);
  assert.ok(feed.retry);
  feed.stop();
});
