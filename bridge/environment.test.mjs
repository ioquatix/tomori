import test from "node:test";
import assert from "node:assert/strict";
import { validEnvironment } from "./environment.mjs";

test("spectator environment accepts built-in presets and legacy omission only", () => {
  for (const value of [undefined, "minimal", "sky"]) assert.equal(validEnvironment(value), true);
  for (const value of [null, [], {}, 0, "unknown", "Sky", { preset: "sky" }]) {
    assert.equal(validEnvironment(value), false);
  }
});
