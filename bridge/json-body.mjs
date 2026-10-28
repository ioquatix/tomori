import { InputError } from "./core.mjs";

export const DEFAULT_JSON_LIMIT = 1024 * 1024;
// Scene frames can contain 64 workers, their geometry, and 16 curated cards.
// Keep normal control/voice requests at 1 MiB; only the scene endpoint has the
// larger bounded budget. This is not a general-purpose upload endpoint.
export const SPECTATOR_JSON_LIMIT = 8 * 1024 * 1024;

export async function bodyOf(request, limit = DEFAULT_JSON_LIMIT) {
  if (!request.headers["content-type"]?.startsWith("application/json")) throw new InputError("JSON required.");
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > limit) throw new InputError("Request too large.");
    chunks.push(chunk);
  }
  try {
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    if (!body || typeof body !== "object" || Array.isArray(body)) throw new Error();
    return body;
  } catch { throw new InputError("Invalid JSON object."); }
}
