import { Readable } from "node:stream";
import { test } from "node:test";
import assert from "node:assert/strict";
import { bodyOf, DEFAULT_JSON_LIMIT, SPECTATOR_JSON_LIMIT } from "./json-body.mjs";
import { presentation } from "./delegation.mjs";

const request = text => Object.assign(Readable.from([Buffer.from(text)]), {
  headers: { "content-type": "application/json" },
});

test("large valid worst-case cards and worker geometry fit the bounded scene budget", async () => {
  const card = i => presentation({ id: `card_${i}`, source_id: "ses_worker", source_message_id: "msg_done",
    title: "t".repeat(160), kind: "table", text: "\u0001".repeat(4096),
    columns: Array(6).fill("\u0001".repeat(80)),
    rows: Array.from({ length: 16 }, () => Array(6).fill("\u0001".repeat(160))) });
  const cube = { position: [-99.123456, 99.123456, -99.123456], scale: [0.123456, 0.123456, 0.123456], color: [0.123456, 0.123456, 0.123456] };
  const frame = JSON.stringify({ worker_cubes: Array(4096).fill(cube),
    workspace: { nodes: [], presentations: Array.from({ length: 16 }, (_, i) => card(i)), error: null, truncated: false } });
  assert.ok(Buffer.byteLength(frame) > DEFAULT_JSON_LIMIT);
  assert.ok(Buffer.byteLength(frame) < SPECTATOR_JSON_LIMIT);
  assert.equal((await bodyOf(request(frame), SPECTATOR_JSON_LIMIT)).workspace.presentations.length, 16);
  await assert.rejects(bodyOf(request(frame)), /too large/);
});

test("scene requests still reject data beyond the larger limit and malformed JSON", async () => {
  await assert.rejects(bodyOf(request(`{"x":"${"x".repeat(SPECTATOR_JSON_LIMIT)}"}`), SPECTATOR_JSON_LIMIT), /too large/);
  await assert.rejects(bodyOf(request("[]"), SPECTATOR_JSON_LIMIT), /Invalid JSON object/);
  await assert.rejects(bodyOf(request("{"), SPECTATOR_JSON_LIMIT), /Invalid JSON object/);
});
