// Optional for older Quest senders. Only observed state, never inferred thought.
const modes = new Set([
  "neutral", "unknown", "working", "preparing", "speaking", "ready",
  "waiting", "listening", "transcribing", "unavailable",
]);

export function validInteraction(value) {
  return value === undefined || (
    value !== null && typeof value === "object" && !Array.isArray(value) &&
    modes.has(value.mode) && typeof value.working === "boolean" &&
    Number.isFinite(value.level) && value.level >= 0 && value.level <= 1 &&
    Number.isFinite(value.time) && value.time >= 0
  );
}
