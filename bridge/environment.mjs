// Fixed, built-in environments only. An omitted field is a legacy Minimal frame.
export function validEnvironment(value) {
  return value === undefined || value === "minimal" || value === "sky";
}
