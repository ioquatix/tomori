import test from "node:test";
import assert from "node:assert/strict";
import { validInteraction } from "./interaction.mjs";

test("conversation state is optional for old senders and validates observed fields", () => {
  assert.equal(validInteraction(undefined), true);
  const state = { mode: "speaking", working: true, level: 0.4, time: 1.5 };
  assert.equal(validInteraction(state), true);
  for (const value of [null, [], {}, { ...state, mode: "thinking" },
    { ...state, working: 1 }, { ...state, level: -0.1 }, { ...state, level: 1.1 },
    { ...state, level: NaN }, { ...state, time: Infinity }, { ...state, time: -1 }]) {
    assert.equal(validInteraction(value), false);
  }
});
