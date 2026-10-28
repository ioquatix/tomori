import { constants } from "node:fs";
import { mkdir, open, rename, unlink } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { createHash, randomUUID } from "node:crypto";

const MAX_NODES = 64;
const MAX_CARDS = 16;
const MAX_STORE_BYTES = 4 * 1024 * 1024;
const CONCURRENCY = 16;
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const reference = value => typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(value);
const sessionRef = value => reference(value) && value.startsWith("ses");
const messageRef = value => reference(value) && value.startsWith("msg_");
const bytes = (value, max) => typeof value === "string" && Buffer.byteLength(value, "utf8") <= max;
const titleOK = value => typeof value === "string" && value.trim() && [...value].length <= 160;
const empty = () => ({ nodes: [], presentations: [], error: null, truncated: false, stale: false });

export class DelegationError extends Error {
  constructor(message, status = 400) { super(message); this.status = status; }
}

function checkRoot(root) {
  if (!sessionRef(root)) throw new DelegationError("Invalid root session reference.");
}

export function presentation(body, generateID = true) {
  const keys = ["id", "source_id", "source_message_id", "title", "kind", "text", "columns", "rows"];
  if (!body || typeof body !== "object" || Array.isArray(body) || Object.keys(body).some(key => !keys.includes(key))) {
    throw new DelegationError("Invalid presentation object.");
  }
  const card = {
    id: body.id === undefined && generateID ? `card_${randomUUID()}` : body.id,
    source_id: body.source_id, source_message_id: body.source_message_id, title: body.title,
    kind: body.kind === undefined ? "markdown" : body.kind,
    text: body.text === undefined ? "" : body.text,
    columns: body.columns === undefined ? [] : body.columns,
    rows: body.rows === undefined ? [] : body.rows,
  };
  if (!reference(card.id) || !sessionRef(card.source_id) || !messageRef(card.source_message_id) || !titleOK(card.title) ||
      !["markdown", "table"].includes(card.kind) || !bytes(card.text, 4096) ||
      !Array.isArray(card.columns) || card.columns.length > 6 || !card.columns.every(column => bytes(column, 80)) ||
      !Array.isArray(card.rows) || card.rows.length > 16 || !card.rows.every(row =>
        Array.isArray(row) && row.length === card.columns.length && row.every(cell => bytes(cell, 160))) ||
      (card.kind === "markdown" && (card.columns.length || card.rows.length)) ||
      (card.kind === "table" && !card.columns.length)) {
    throw new DelegationError("Invalid presentation fields or bounds.");
  }
  return structuredClone(card);
}

// Single bridge writer. Every mutation re-reads and validates the whole bounded
// store; a corrupt/unreadable file is never treated as an empty store.
export class PresentationStore {
  constructor(path = resolve(".space/presentations.json")) {
    this.path = path;
    this.memory = { version: 1, roots: [] };
    this.queue = Promise.resolve();
  }

  async load() {
    if (this.path === null) return structuredClone(this.memory);
    let file;
    try {
      file = await open(this.path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      const stat = await file.stat();
      if (!stat.isFile() || stat.size > MAX_STORE_BYTES) throw new Error();
      // Bound the read even if the file grows after stat().
      const buffer = Buffer.alloc(stat.size + 1);
      let size = 0;
      while (size < buffer.length) {
        const { bytesRead } = await file.read(buffer, size, buffer.length - size, size);
        if (!bytesRead) break;
        size += bytesRead;
      }
      if (size > stat.size) throw new Error();
      const data = JSON.parse(buffer.subarray(0, size).toString("utf8"));
      if (data?.version !== 1 || !Array.isArray(data.roots) || data.roots.length > 32 ||
          Object.keys(data).some(key => !["version", "roots"].includes(key))) throw new Error();
      const seen = new Set(); let total = 0;
      for (const root of data.roots) {
        if (!root || !sessionRef(root.id) || seen.has(root.id) || Object.keys(root).some(key => !["id", "presentations"].includes(key)) ||
            !Array.isArray(root.presentations) || root.presentations.length > MAX_CARDS) throw new Error();
        seen.add(root.id); total += root.presentations.length;
        const ids = new Set();
        root.presentations = root.presentations.map(card => {
          if (Object.keys(card).length !== 8) throw new Error();
          const valid = presentation(card, false);
          if (ids.has(valid.id)) throw new Error();
          ids.add(valid.id); return valid;
        });
      }
      if (total > 128) throw new Error();
      return data;
    } catch (error) {
      if (error.code === "ENOENT") return { version: 1, roots: [] };
      throw new DelegationError("Presentation store unavailable or corrupt.", 503);
    } finally { await file?.close(); }
  }

  async list(root) {
    checkRoot(root);
    const data = await this.load();
    return (data.roots.find(item => item.id === root)?.presentations || []).sort((a, b) => compare(a.id, b.id));
  }

  mutate(root, update) {
    const operation = this.queue.then(async () => {
      const data = await this.load();
      let entry = data.roots.find(item => item.id === root);
      const cards = update(entry?.presentations || []);
      if (cards.length > MAX_CARDS) throw new DelegationError("Presentation limit reached.", 409);
      if (!entry && cards.length) {
        entry = { id: root, presentations: [] }; data.roots.push(entry);
      }
      if (entry) entry.presentations = cards.sort((a, b) => compare(a.id, b.id));
      data.roots = data.roots.filter(item => item.presentations.length).sort((a, b) => compare(a.id, b.id));
      if (data.roots.length > 32 || data.roots.reduce((n, item) => n + item.presentations.length, 0) > 128) {
        throw new DelegationError("Presentation store limit reached.", 409);
      }
      if (this.path === null) { this.memory = data; return; }
      const text = JSON.stringify(data);
      if (Buffer.byteLength(text) > MAX_STORE_BYTES) throw new DelegationError("Presentation store limit reached.", 409);
      const temporary = `${this.path}.${randomUUID()}.tmp`;
      let file; let created = false;
      try {
        await mkdir(dirname(this.path), { recursive: true });
        file = await open(temporary, "wx", 0o600);
        created = true;
        await file.writeFile(text, "utf8"); await file.sync(); await file.close(); file = null;
        await rename(temporary, this.path);
      } catch {
        throw new DelegationError("Presentation store write failed.", 503);
      } finally {
        await file?.close();
        if (created) await unlink(temporary).catch(() => {});
      }
    });
    this.queue = operation.catch(() => {});
    return operation;
  }

  async put(root, card) {
    checkRoot(root);
    card = presentation(card, false);
    await this.mutate(root, cards => [...cards.filter(item => item.id !== card.id), card]);
    return card;
  }

  async remove(root, id) {
    checkRoot(root);
    if (!reference(id)) throw new DelegationError("Invalid presentation reference.");
    await this.mutate(root, cards => cards.filter(card => card.id !== id));
    return { ok: true };
  }
}

// Separate from reviewed cards; records are decisions about an observed outcome,
// not session deletion. As with PresentationStore, one bridge owns the writer.
export class VisibilityStore {
  constructor(path = resolve(".space/worker-visibility.json")) {
    this.path = path; this.memory = { version: 1, roots: [] }; this.queue = Promise.resolve();
  }

  async load() {
    if (this.path === null) return structuredClone(this.memory);
    let file;
    try {
      file = await open(this.path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      const stat = await file.stat();
      if (!stat.isFile() || stat.size > MAX_STORE_BYTES || (stat.mode & 0o7777) !== 0o600) throw new Error();
      const buffer = Buffer.alloc(stat.size + 1); let size = 0;
      while (size < buffer.length) {
        const { bytesRead } = await file.read(buffer, size, buffer.length - size, size);
        if (!bytesRead) break;
        size += bytesRead;
      }
      if (size > stat.size) throw new Error();
      const data = JSON.parse(buffer.subarray(0, size).toString("utf8"));
      if (data?.version !== 1 || !Array.isArray(data.roots) || data.roots.length > 32 ||
          Object.keys(data).length !== 2) throw new Error();
      const roots = new Set();
      for (const root of data.roots) {
        if (!root || Object.keys(root).length !== 2 || !sessionRef(root.id) || roots.has(root.id) ||
            !Array.isArray(root.workers) || root.workers.length > MAX_NODES) throw new Error();
        roots.add(root.id); const ids = new Set();
        for (const item of root.workers) {
          if (!item || Object.keys(item).length !== 6 || !sessionRef(item.id) || item.id === root.id || ids.has(item.id) ||
              !["completed", "error"].includes(item.state) || !messageRef(item.latest_id) || !messageRef(item.evidence_id) ||
              typeof item.digest !== "string" || !/^[a-f0-9]{64}$/.test(item.digest) || typeof item.superseded !== "boolean") throw new Error();
          ids.add(item.id);
        }
      }
      return data;
    } catch (error) {
      if (error.code === "ENOENT") return { version: 1, roots: [] };
      throw new DelegationError("Worker visibility store unavailable or corrupt.", 503);
    } finally { await file?.close(); }
  }

  async list(root) {
    checkRoot(root);
    const data = await this.load();
    return (data.roots.find(item => item.id === root)?.workers || []).sort((a, b) => compare(a.id, b.id));
  }

  mutate(root, update) {
    checkRoot(root);
    const operation = this.queue.then(async () => {
      const data = await this.load();
      let entry = data.roots.find(item => item.id === root);
      const workers = update(entry?.workers || []);
      if (workers === null) return;
      if (workers.length > MAX_NODES) throw new DelegationError("Worker visibility limit reached (64 per root); restore old records first.", 409);
      if (!entry && workers.length) { entry = { id: root, workers: [] }; data.roots.push(entry); }
      if (entry) entry.workers = workers.sort((a, b) => compare(a.id, b.id));
      data.roots = data.roots.filter(item => item.workers.length).sort((a, b) => compare(a.id, b.id));
      if (data.roots.length > 32) throw new DelegationError("Worker visibility store limit reached (32 roots); restore old records first.", 409);
      if (this.path === null) { this.memory = data; return; }
      const text = JSON.stringify(data);
      if (Buffer.byteLength(text) > MAX_STORE_BYTES) throw new DelegationError("Worker visibility store limit reached.", 409);
      const temporary = `${this.path}.${randomUUID()}.tmp`; let file; let created = false;
      try {
        await mkdir(dirname(this.path), { recursive: true });
        file = await open(temporary, "wx", 0o600); created = true;
        await file.chmod(0o600);
        await file.writeFile(text, "utf8"); await file.sync(); await file.close(); file = null;
        await rename(temporary, this.path);
      } catch { throw new DelegationError("Worker visibility store write failed.", 503); }
      finally { await file?.close(); if (created) await unlink(temporary).catch(() => {}); }
    });
    this.queue = operation.catch(() => {}); return operation;
  }

  put(root, record, verify = () => {}) {
    return this.mutate(root, workers => { verify(); return [...workers.filter(item => item.id !== record.id), record]; });
  }

  supersede(root, candidates, verify) {
    return this.mutate(root, workers => {
      if (!verify()) return null;
      let changed = false;
      const next = workers.map(item => {
        const candidate = candidates.get(item.id);
        if (!item.superseded && sameEvidence(item, candidate)) { changed = true; return { ...item, superseded: true }; }
        return item;
      });
      return changed ? next : null;
    });
  }

  async remove(root, worker) {
    if (!sessionRef(worker) || worker === root) throw new DelegationError("Invalid worker session reference.");
    await this.mutate(root, workers => workers.filter(item => item.id !== worker));
    return { ok: true };
  }
}

// Global network concurrency limit, including provenance checks. Queued calls
// are cancelled by the overall operation deadline, not just a socket timeout.
class Limiter {
  constructor(limit) { this.limit = limit; this.active = 0; this.waiters = []; }
  async run(work, signal) {
    if (signal.aborted) throw new Error();
    if (this.active >= this.limit) await new Promise((resolve, reject) => {
      if (this.waiters.length >= 64) { reject(new Error()); return; }
      const waiter = () => { signal.removeEventListener("abort", cancel); resolve(); };
      const cancel = () => { this.waiters = this.waiters.filter(item => item !== waiter); reject(new Error()); };
      this.waiters.push(waiter); signal.addEventListener("abort", cancel, { once: true });
    });
    else this.active++;
    try { if (signal.aborted) throw new Error(); return await work(); }
    finally { const next = this.waiters.shift(); if (next) next(); else this.active--; }
  }
}

async function mapBounded(items, work) {
  let index = 0;
  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, items.length) }, async () => {
    while (index < items.length) { const i = index++; await work(items[i], i); }
  }));
}

function observedState(session, messages, permissions, forms, inbox, active) {
  if (permissions.length || forms.length) return "waiting";
  if (!active) return "unknown";
  if (active[session.id]) return "running";
  if (inbox.length) return "idle";
  const lastEvent = messages.data.find(message => ["idle", "assistant", "user", "synthetic", "shell", "compaction"].includes(message.type));
  if (lastEvent?.type === "idle" && (lastEvent.error || lastEvent.outcome === "failed")) return "error";
  if (lastEvent?.type === "idle" && lastEvent.outcome === "interrupted") return "idle";
  const latest = messages.data.find(message => ["assistant", "user", "synthetic", "shell", "compaction"].includes(message.type));
  if (latest?.error || latest?.status === "failed" || latest?.finish === "error" || session.outcome === "failed") return "error";
  if (session.outcome === "interrupted") return "idle";
  if (latest?.type !== "assistant") return latest ? "idle" : "unknown";
  const tools = latest.content.filter(part => part.type === "tool");
  if (tools.some(part => part.state?.status === "error")) return "error";
  if (tools.some(part => !["completed", "error"].includes(part.state?.status))) return "unknown";
  // A settled tool step, length limit, or unfinished stream is not a final reply.
  return !tools.length && latest.time?.completed && latest.finish === "stop" && latest.content.some(part => part.type === "text" && part.text.trim())
    ? "completed" : "idle";
}

function terminalEvidence(state, messages) {
  if (!["completed", "error"].includes(state) || !Array.isArray(messages.data) || !messages.data.length) return null;
  const types = ["idle", "assistant", "user", "synthetic", "shell", "compaction"];
  const index = messages.data.findIndex(message => types.includes(message?.type));
  if (index < 0 || !messages.data.slice(0, index + 1).every(message => messageRef(message?.id) && typeof message.type === "string")) return null;
  let event = messages.data[index];
  // An idle event can follow the relevant work. Retain both IDs: the head
  // detects new events and the evidence identifies the actual terminal result.
  if (event.type === "idle" && !(state === "error" && (event.error || event.outcome === "failed"))) {
    const replyIndex = messages.data.findIndex((message, i) => i > index && ["assistant", "user", "synthetic", "shell", "compaction"].includes(message?.type));
    if (replyIndex < 0 || !messages.data.slice(index + 1, replyIndex + 1).every(message => messageRef(message?.id) && typeof message.type === "string")) return null;
    event = messages.data[replyIndex];
  }
  if (state === "completed" && event.type !== "assistant") return null;
  if (state === "error" && !["idle", "assistant", "shell", "compaction"].includes(event.type)) return null;
  if (state === "error" && event.type === "assistant") {
    if (!Array.isArray(event.content) || event.content.some(part => part?.type === "tool" &&
        !["completed", "error"].includes(part.state?.status))) return null;
    if (!(event.error || event.status === "failed" || event.finish === "error" ||
        event.content.some(part => part?.type === "tool" && part.state?.status === "error"))) return null;
  }
  if (state === "error" && event.type === "shell" && !(event.status === "timeout" || event.status === "killed" ||
      (event.status === "exited" && typeof event.exit === "number" && event.exit !== 0))) return null;
  if (state === "error" && event.type === "compaction" && event.status !== "failed") return null;
  try {
    const text = JSON.stringify([messages.data[0], event]);
    if (!bytes(text, MAX_STORE_BYTES)) return null;
    return { state, latest_id: messages.data[0].id, evidence_id: event.id,
      digest: createHash("sha256").update(text).digest("hex") };
  } catch { return null; }
}

const sameEvidence = (a, b) => !!a && !!b && a.state === b.state && a.latest_id === b.latest_id && a.evidence_id === b.evidence_id && a.digest === b.digest;

export class Delegation {
  constructor(client, { storePath, store = new PresentationStore(storePath), ttl = 1000, maxAge = 3000, timeout = 1500,
    visibilityPath, visibilityStore = new VisibilityStore(visibilityPath === undefined && storePath === null ? null : visibilityPath),
    refreshTimeout = 10000, now = () => performance.now() } = {}) {
    this.client = client; this.store = store; this.ttl = ttl; this.maxAge = maxAge; this.timeout = timeout;
    this.visibilityStore = visibilityStore;
    this.refreshTimeout = refreshTimeout; this.now = now;
    this.cache = new Map(); this.limiter = new Limiter(CONCURRENCY);
  }

  call(work, signal) {
    const bounded = AbortSignal.any([signal, AbortSignal.timeout(this.timeout)]);
    // Keep the limiter slot until the transport actually settles, even if it
    // ignores cancellation. The caller still receives a bounded response.
    const pending = this.limiter.run(() => work({ signal: bounded }), bounded);
    return new Promise((resolve, reject) => {
      const abort = () => reject(new Error());
      if (bounded.aborted) abort();
      else bounded.addEventListener("abort", abort, { once: true });
      pending.then(value => { bounded.removeEventListener("abort", abort); resolve(value); },
        error => { bounded.removeEventListener("abort", abort); reject(error); });
    });
  }

  entry(root) {
    let entry = this.cache.get(root);
    if (!entry) {
      if (this.cache.size >= 16) {
        const oldest = [...this.cache].filter(([, value]) => !value.pending).sort((a, b) => a[1].at - b[1].at)[0];
        if (!oldest) throw new DelegationError("Delegation cache busy.", 503);
        this.cache.delete(oldest[0]);
      }
      entry = { at: -Infinity, retryAt: -Infinity, value: { ...empty(), stale: true }, metadata: new Map(),
        hidden: new Set(), invalidated: new Map(), revision: 0, pending: null }; this.cache.set(root, entry);
    }
    return entry;
  }

  async get(root) {
    checkRoot(root);
    try {
      const entry = this.entry(root);
      if (this.now() - entry.at >= this.ttl && this.now() >= entry.retryAt) this.refresh(root);
      const value = structuredClone(entry.value);
      const observedValue = entry.value, metadata = entry.metadata, previouslyHidden = entry.hidden;
      const observedAt = entry.at;
      try { value.presentations = await this.store.list(root); }
      catch { value.error = "Presentation store unavailable or corrupt."; }
      let records = [];
      try { records = await this.visibilityStore.list(root); }
      catch { value.error = value.error || "Worker visibility store unavailable or corrupt."; }
      try { await this.observeChanges(root, entry, observedValue, metadata, observedAt, records); }
      catch { value.error = value.error || "Worker visibility store write failed."; }
      // Refresh ahead of expiry: otherwise every routine refresh turns workers
      // unknown for a whole headset poll, causing visible status flicker.
      // Check after the store read as it can itself delay the response.
      const stale = value.stale || this.now() - observedAt >= this.maxAge;
      const byID = new Map(records.map(record => [record.id, record]));
      const children = new Map();
      for (const node of value.nodes) {
        if (!children.has(node.parent_id)) children.set(node.parent_id, []);
        children.get(node.parent_id).push(node);
      }
      const hidden = new Set();
      const visit = node => {
        const descendantsHidden = (children.get(node.id) || []).map(visit).every(Boolean);
        const meta = metadata.get(node.id);
        const record = byID.get(node.id);
        const eligible = meta?.childrenComplete && !record?.superseded &&
          !sameEvidence(entry.invalidated.get(node.id), record) && sameEvidence(meta.fingerprint, record) && descendantsHidden;
        if (eligible && (!stale || previouslyHidden.has(node.id))) { hidden.add(node.id); return true; }
        return false;
      };
      for (const node of children.get(root) || []) visit(node);
      if (!stale && entry.value === observedValue) entry.hidden = hidden;
      value.nodes = value.nodes.filter(node => !hidden.has(node.id));
      if (stale) {
        value.stale = true;
        value.nodes = value.nodes.map(node => ({ ...node, state: "unknown" }));
        value.error = value.error || "Delegation refreshing; status is stale.";
      }
      return value;
    } catch { return { ...empty(), stale: true, error: "Delegation unavailable." }; }
  }

  async observeChanges(root, entry, value, metadata, at, suppliedRecords) {
    const revision = entry.revision;
    const valid = () => entry.value === value && entry.revision === revision && !value.stale && this.now() - at < this.maxAge;
    if (!valid()) return;
    const records = suppliedRecords || await this.visibilityStore.list(root);
    if (!valid()) return;
    const byNode = new Map(value.nodes.map(node => [node.id, node]));
    const candidates = new Map();
    const recordsByID = new Map(records.map(record => [record.id, record]));
    for (const [id, invalidated] of entry.invalidated) {
      const record = recordsByID.get(id);
      if (!sameEvidence(invalidated, record) || record.superseded) entry.invalidated.delete(id);
      else candidates.set(id, invalidated);
    }
    for (const record of records) {
      const node = byNode.get(record.id), fingerprint = metadata.get(record.id)?.fingerprint;
      if (!record.superseded && node && node.state !== "unknown" &&
          (!["completed", "error"].includes(node.state) || (fingerprint && !sameEvidence(record, fingerprint)))) {
        candidates.set(record.id, record); entry.invalidated.set(record.id, record);
      }
    }
    if (candidates.size) await this.visibilityStore.supersede(root, candidates, valid);
  }

  refresh(root) {
    checkRoot(root);
    const entry = this.entry(root);
    if (entry.pending) return entry.pending;
    // A projection combines many calls. Its oldest possible evidence is from
    // the start, not the time the final request completes.
    const startedAt = this.now();
    entry.pending = this.project(root, AbortSignal.timeout(this.refreshTimeout)).then(async ({ value, metadata, hierarchyIncomplete }) => {
      // Partial discovery cannot certify that previously seen workers vanished.
      if (hierarchyIncomplete) {
        const previous = new Set(entry.value.nodes.map(node => node.id));
        const seen = new Set(value.nodes.map(node => node.id));
        for (const old of entry.value.nodes) {
          if (!seen.has(old.id) && (old.parent_id === root || seen.has(old.parent_id))) {
            value.nodes.push({ ...old, state: "unknown" }); seen.add(old.id);
            metadata.set(old.id, { fingerprint: null, childrenComplete: false });
            const parent = metadata.get(old.parent_id); if (parent) parent.childrenComplete = false;
          }
        }
        // A partial new page and the previous tree may together exceed the
        // projection bound. Prioritize observed active/waiting workers and
        // their ancestry, then previously seen nodes. Report truncation rather
        // than silently exceeding 64.
        if (value.nodes.length > MAX_NODES) {
          const nodesByID = new Map(value.nodes.map(node => [node.id, node]));
          const urgent = new Set();
          for (const node of value.nodes.filter(item => ["running", "waiting"].includes(item.state))) {
            let current = node;
            while (current && !urgent.has(current.id)) { urgent.add(current.id); current = nodesByID.get(current.parent_id); }
          }
          const remaining = [...value.nodes]; const chosen = []; const parents = new Set([root]);
          while (chosen.length < MAX_NODES) {
            const available = remaining.filter(node => parents.has(node.parent_id)).sort((a, b) =>
              Number(urgent.has(b.id)) - Number(urgent.has(a.id)) ||
              Number(previous.has(b.id)) - Number(previous.has(a.id)) || compare(a.id, b.id));
            if (!available.length) break;
            const node = available[0]; remaining.splice(remaining.indexOf(node), 1); chosen.push(node); parents.add(node.id);
          }
          for (const omitted of remaining) {
            const parent = metadata.get(omitted.parent_id); if (parent) parent.childrenComplete = false;
          }
          value.nodes = chosen;
        }
        const byParent = new Map();
        for (const node of value.nodes) {
          if (!byParent.has(node.parent_id)) byParent.set(node.parent_id, []);
          byParent.get(node.parent_id).push(node);
        }
        value.nodes = [];
        const append = parent => { for (const node of (byParent.get(parent) || []).sort((a, b) => compare(a.id, b.id))) {
          value.nodes.push(node); append(node.id);
        } };
        append(root);
      }
      entry.value = value; entry.metadata = metadata;
      entry.at = startedAt;
      entry.retryAt = -Infinity;
      try { await this.observeChanges(root, entry, value, metadata, startedAt); }
      catch { /* The next read reports the store error and keeps workers visible. */ }
    }, () => {
      entry.value = { ...empty(), nodes: entry.value.nodes.map(node => ({ ...node, state: "unknown" })),
        truncated: entry.value.truncated, stale: true, error: "Delegation unavailable; status is unknown." };
      entry.at = -Infinity;
      entry.retryAt = this.now() + this.ttl;
    }).finally(() => { entry.pending = null; });
    return entry.pending;
  }

  async project(root, signal) {
    const client = this.client;
    const rootSession = await this.call(options => client.session.get({ sessionID: root }, options), signal);
    if (rootSession?.id !== root) throw new Error();
    const value = empty();
    const sessions = new Map(); const seen = new Set([root]); const completeChildren = new Map();
    let frontier = [root]; let pageBudget = 128;
    while (frontier.length) {
      const lists = new Map();
      await mapBounded(frontier, async parentID => {
        const children = []; let cursor; let receivedPage = false; let complete = true; const cursors = new Set();
        try {
          do {
            if (--pageBudget < 0) { value.truncated = true; complete = false; break; }
            const page = await this.call(options => client.session.list(cursor ? { limit: 65, cursor } :
              { limit: 65, parentID, order: "asc" }, options), signal);
            if (!Array.isArray(page.data)) throw new Error();
            receivedPage = true;
            for (const child of page.data.slice(0, 65)) {
              if (!sessionRef(child?.id) || child.parentID !== parentID || seen.has(child.id)) {
                value.error = "Invalid delegation hierarchy omitted."; value.truncated = true; complete = false; continue;
              }
              children.push(child);
            }
            if (page.data.length > 65) { value.truncated = true; complete = false; }
            cursor = page.cursor?.next;
            if (cursor && (typeof cursor !== "string" || cursor.length > 8192 || cursors.has(cursor))) {
              value.error = "Invalid delegation pagination."; value.truncated = true; complete = false; break;
            }
            if (cursor) cursors.add(cursor);
            if (children.length > MAX_NODES) { value.truncated = true; complete = false; break; }
          } while (cursor);
        } catch (error) {
          if (parentID === root && !receivedPage) throw error;
          value.error = "Some delegation hierarchy is unavailable."; value.truncated = true; complete = false;
        }
        lists.set(parentID, children.sort((a, b) => compare(a.id, b.id)));
        completeChildren.set(parentID, complete);
      });
      const next = [];
      for (const parentID of frontier) for (const session of lists.get(parentID)) {
        if (seen.has(session.id)) { value.error = "Invalid delegation hierarchy omitted."; value.truncated = true; completeChildren.set(parentID, false); continue; }
        if (sessions.size >= MAX_NODES) { value.truncated = true; completeChildren.set(parentID, false); continue; }
        seen.add(session.id); sessions.set(session.id, session); next.push(session.id);
      }
      frontier = next;
    }
    const evidence = new Map();
    let active = null;
    await Promise.all([mapBounded([...sessions.values()], async session => {
      const sessionID = session.id;
      try {
        // Sequential inside a worker keeps the global request queue bounded.
        const permissions = await this.call(options => client.permission.list({ sessionID }, options), signal);
        const forms = await this.call(options => client.session.form.list({ sessionID }, options), signal);
        const inbox = await this.call(options => client.session.inbox.list({ sessionID }, options), signal);
        const messages = await this.call(options => client.message.list({ sessionID, limit: 16, order: "desc" }, options), signal);
        if (!Array.isArray(permissions) || !Array.isArray(forms) || !Array.isArray(inbox) || !Array.isArray(messages.data)) throw new Error();
        if (messages.cursor?.next && !messages.data.some(message => ["assistant", "user", "synthetic", "shell", "compaction"].includes(message.type))) {
          value.truncated = true;
        }
        evidence.set(sessionID, { permissions, forms, inbox, messages });
      } catch { value.error = "Some delegation status is unavailable."; }
    }), (async () => {
      try {
        active = await this.call(options => client.session.active(options), signal);
        if (!active || typeof active !== "object" || Array.isArray(active)) throw new Error();
      } catch { active = null; value.error = "Some delegation status is unavailable."; }
    })()]);
    const metadata = new Map();
    const append = parentID => {
      for (const session of [...sessions.values()].filter(item => item.parentID === parentID).sort((a, b) => compare(a.id, b.id))) {
        const facts = evidence.get(session.id);
        let state = "unknown";
        try { if (facts) state = observedState(session, facts.messages, facts.permissions, facts.forms, facts.inbox, active); }
        catch { value.error = "Some delegation status is unavailable."; }
        metadata.set(session.id, { fingerprint: facts ? terminalEvidence(state, facts.messages) : null,
          childrenComplete: completeChildren.get(session.id) === true, outcome: session.outcome });
        value.nodes.push({ id: session.id, parent_id: parentID,
          title: [...(typeof session.title === "string" ? session.title : session.id)].slice(0, 160).join(""), state });
        append(session.id);
      }
    };
    append(root);
    return { value, metadata, hierarchyIncomplete: [...completeChildren.values()].some(complete => !complete) };
  }

  async acknowledge(root, worker) {
    checkRoot(root);
    if (!sessionRef(worker) || worker === root) throw new DelegationError("Invalid worker session reference.");
    const entry = this.entry(root);
    // Do not approve based only on an earlier cached poll or an in-flight poll.
    if (entry.pending) await entry.pending;
    await this.refresh(root);
    const observedValue = entry.value, observedAt = entry.at;
    const node = observedValue.nodes.find(item => item.id === worker);
    const meta = entry.metadata.get(worker);
    if (observedValue.stale || this.now() - observedAt >= this.maxAge) {
      throw new DelegationError("Fresh worker status could not be verified.", 409);
    }
    if (!node || !meta?.fingerprint || !["completed", "error"].includes(node.state)) {
      throw new DelegationError("Worker is not a verified terminal descendant in the current projection.", 409);
    }
    const parents = new Map(observedValue.nodes.map(item => [item.id, item.parent_id]));
    const signal = AbortSignal.timeout(this.refreshTimeout);
    try {
      let id = worker; const seen = new Set();
      while (id !== root) {
        if (!sessionRef(id) || seen.has(id) || seen.size >= MAX_NODES || !parents.has(id)) throw new Error();
        seen.add(id);
        const session = await this.call(options => this.client.session.get({ sessionID: id }, options), signal);
        if (session?.id !== id || session.parentID !== parents.get(id) || (id === worker && session.outcome !== meta.outcome)) throw new Error();
        id = session.parentID;
      }
      const rootSession = await this.call(options => this.client.session.get({ sessionID: root }, options), signal);
      if (rootSession?.id !== root) throw new Error();
    } catch { throw new DelegationError("Worker ancestry or terminal evidence could not be verified.", 422); }
    const record = { id: worker, ...meta.fingerprint, superseded: false };
    await this.visibilityStore.put(root, record, () => {
      if (entry.value !== observedValue || this.now() - observedAt >= this.maxAge) {
        throw new DelegationError("Fresh worker status could not be verified; retry.", 409);
      }
      entry.revision++;
    });
    entry.invalidated.delete(worker);
    return { ok: true, worker_id: worker, state: record.state };
  }

  async acknowledged(root) {
    return (await this.visibilityStore.list(root)).map(({ id, state, superseded }) => ({ worker_id: id, state, superseded }));
  }

  restore(root, worker) {
    checkRoot(root);
    return this.visibilityStore.remove(root, worker);
  }

  async publish(root, body) {
    checkRoot(root);
    const card = presentation(body);
    const signal = AbortSignal.timeout(this.refreshTimeout);
    try {
      // Walk the real parent chain rather than trusting the bounded visual tree.
      // Archived children remain valid sources. No message content is harvested.
      let id = card.source_id; const seen = new Set(); let member = false;
      for (let depth = 0; depth <= MAX_NODES; depth++) {
        if (!sessionRef(id) || seen.has(id)) break;
        seen.add(id);
        const session = await this.call(options => this.client.session.get({ sessionID: id }, options), signal);
        if (session.id !== id) break;
        if (id === root) { member = true; break; }
        id = session.parentID;
      }
      if (!member) throw new DelegationError("Presentation source is not in this workspace.");
      const message = await this.call(options => this.client.session.message.get({ sessionID: card.source_id,
        messageID: card.source_message_id }, options), signal);
      if (message.id !== card.source_message_id) throw new Error();
    } catch (error) {
      if (error instanceof DelegationError) throw error;
      throw new DelegationError("Presentation provenance could not be verified.", 422);
    }
    return this.store.put(root, card);
  }

  remove(root, id) {
    checkRoot(root);
    if (!reference(id)) throw new DelegationError("Invalid presentation reference.");
    return this.store.remove(root, id);
  }
}
