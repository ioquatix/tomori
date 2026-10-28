import assert from "node:assert/strict";
import { test } from "node:test";
import { validControllerPoses, validHands } from "./controller-poses.mjs";

test("accepts old frames and valid left/right poses", () => {
  assert.equal(validControllerPoses(undefined), true);
  assert.equal(validControllerPoses([null, { position: [1, 2, 3], orientation: [0, 0, 0, 1] }]), true);
});

test("validates optional hand joints and rejects malformed geometry", () => {
  const hand = { joints: Array(26).fill(null) };
  hand.joints[1] = { position: [1, 2, 3], radius: 0.01 };
  assert.equal(validHands(undefined), true);
  assert.equal(validHands([hand, null]), true);
  assert.equal(validHands([hand]), false);
  assert.equal(validHands([{ joints: [] }, null]), false);
  const invalid = { joints: [...hand.joints] };
  invalid.joints[1] = { position: [0, Infinity, 0], radius: 0.01 };
  assert.equal(validHands([invalid, null]), false);
  invalid.joints[1] = { position: [0, 0, 0], radius: 0 };
  assert.equal(validHands([invalid, null]), false);
});

test("rejects malformed, non-finite, or unbounded poses", () => {
  for (const poses of [null, [], [null], [null, null, null], [{ position: [0, 0, 0], orientation: [0, 0, 0, 0] }, null],
    [{ position: [Infinity, 0, 0], orientation: [0, 0, 0, 1] }, null],
    [{ position: [10000, 0, 0], orientation: [0, 0, 0, 1] }, null]]) {
    assert.equal(validControllerPoses(poses), false);
  }
});
