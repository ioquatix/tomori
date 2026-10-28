import assert from "node:assert/strict";
import { test } from "node:test";
import { chmod, mkdir, mkdtemp, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { OpenCode } from "@opencode/client";
import { Delegation, DelegationError, PresentationStore, VisibilityStore, presentation } from "./delegation.mjs";

const child = (id, parentID = "ses_root", extra = {}) => ({ id, parentID, title: id, ...extra });
const final = (extra = {}) => ({ id: "msg_final", type: "assistant", finish: "stop", time: { completed: 100 },
  content: [{ type: "reasoning", text: "PRIVATE reasoning" }, { type: "text", text: "PRIVATE worker response" }], ...extra });
const card = (extra = {}) => ({ id: "card_one", source_id: "ses_a", source_message_id: "msg_final",
  title: "Reviewed summary", kind: "markdown", text: "Coordinator supplied", columns: [], rows: [], ...extra });

function fixture(nodes = [child("ses_a")], options = {}) {
  const sessions = new Map([{ id: "ses_root" }, ...nodes].map(session => [session.id, session]));
  const calls = [];
  const facts = { active: {}, permissions: {}, forms: {}, inbox: {}, messages: {} };
  const client = {
    session: {
      get: async ({ sessionID }) => {
        calls.push(["get", sessionID]);
        if (!sessions.has(sessionID)) throw new Error("PRIVATE missing session");
        return sessions.get(sessionID);
      },
      list: async input => {
        calls.push(["list", input]);
        return { data: [...sessions.values()].filter(session => session.parentID === input.parentID).reverse(), cursor: {} };
      },
      active: async () => facts.active,
      form: { list: async ({ sessionID }) => facts.forms[sessionID] || [] },
      inbox: { list: async ({ sessionID }) => facts.inbox[sessionID] || [] },
      message: { get: async ({ sessionID, messageID }) => {
        calls.push(["message", sessionID, messageID]);
        if (messageID !== "msg_final") throw new Error("PRIVATE missing message");
        return final();
      } },
    },
    message: { list: async ({ sessionID }) => ({ data: facts.messages[sessionID] || [final()], cursor: {} }) },
    permission: { list: async ({ sessionID }) => facts.permissions[sessionID] || [] },
  };
  let clock = 0;
  const delegation = new Delegation(client, { storePath: null, visibilityPath: null, now: () => clock, ...options });
  return { client, delegation, facts, sessions, calls, advance: ms => { clock += ms; } };
}

test("projects sorted depth-first descendants, excluding root and raw transcripts", async () => {
  const f = fixture([child("ses_z"), child("ses_b"), child("ses_a"), child("ses_aa", "ses_a"), child("ses_foreign", "ses_other")]);
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => node.id), ["ses_a", "ses_aa", "ses_b", "ses_z"]);
  assert.equal(result.nodes[1].parent_id, "ses_a");
  assert.ok(result.nodes.every(node => node.state === "completed"));
  assert.deepEqual(Object.keys(result), ["nodes", "presentations", "error", "truncated", "stale"]);
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE|content|reasoning/);
  assert.equal(result.error, null);
  assert.equal(result.truncated, false);
  assert.equal(result.stale, false);
});

test("pending permission/form takes precedence over active; active over old completion", async () => {
  const f = fixture([child("ses_a"), child("ses_b"), child("ses_c"), child("ses_d")]);
  f.facts.active = { ses_a: {}, ses_b: {}, ses_c: {} };
  f.facts.permissions.ses_a = [{ id: "permission" }];
  f.facts.forms.ses_b = [{ id: "form" }];
  f.facts.inbox.ses_d = [{ type: "user", payload: { text: "PRIVATE" } }];
  await f.delegation.refresh("ses_root");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.state), ["waiting", "waiting", "running", "idle"]);
});

test("completion requires a completed final reply, not a tool step, new input, interruption, or error", async () => {
  const f = fixture(["a", "b", "c", "d", "e", "f", "g", "h", "i"].map(id => child(`ses_${id}`)));
  f.facts.messages.ses_a = [final({ finish: "tool-calls" })];
  f.facts.messages.ses_b = [final({ time: {} })];
  f.facts.messages.ses_c = [{ type: "user", text: "PRIVATE" }, final()];
  f.facts.messages.ses_d = [final({ error: { message: "PRIVATE error" } })];
  f.facts.messages.ses_e = [final({ content: [{ type: "tool", state: { status: "running" } }] })];
  f.facts.messages.ses_f = [final({ content: [{ type: "tool", state: { status: "error" } }] })];
  f.sessions.get("ses_g").outcome = "interrupted";
  f.sessions.get("ses_h").outcome = "failed";
  f.facts.messages.ses_i = [final({ content: [{ type: "text", text: "PRIVATE" }, { type: "tool", state: { status: "completed" } }] })];
  await f.delegation.refresh("ses_root");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.state),
    ["idle", "idle", "idle", "error", "unknown", "error", "idle", "error", "idle"]);
});

test("cycles, invalid references, and unrelated sessions are omitted", async () => {
  const f = fixture();
  f.client.session.list = async ({ parentID }) => ({ data: parentID === "ses_root" ?
    [child("ses_a"), child("ses_foreign", "ses_unrelated"), child("ses_bad/path")] :
    [child("ses_root", "ses_a"), child("ses_a", "ses_a")], cursor: {} });
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => node.id), ["ses_a"]);
  assert.match(result.error, /hierarchy/);
});

test("follows opaque next cursors without order/filter, and bounds the tree at 64", async () => {
  const nodes = Array.from({ length: 70 }, (_, i) => child(`ses_${String(i).padStart(3, "0")}`));
  const f = fixture(nodes);
  const requests = [];
  f.client.session.list = async input => {
    requests.push(input);
    if (input.cursor === "next-page") return { data: nodes.slice(40), cursor: {} };
    if (input.parentID === "ses_root") return { data: nodes.slice(0, 40), cursor: { next: "next-page" } };
    return { data: [], cursor: {} };
  };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.nodes.length, 64);
  assert.equal(result.truncated, true);
  assert.equal(result.stale, false);
  assert.ok(result.nodes.every(node => node.state === "completed"));
  assert.deepEqual(requests[1], { limit: 65, cursor: "next-page" });
  assert.equal(result.nodes[63].id, "ses_063");
});

test("repeated pagination cursors are bounded and surface truncation", async () => {
  const f = fixture(); let calls = 0;
  f.client.session.list = async () => { calls++; return { data: [], cursor: { next: "loop" } }; };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(calls, 2);
  assert.equal(result.truncated, true);
  assert.match(result.error, /pagination/);
});

test("empty message evidence is unknown; limited non-work history surfaces truncation", async () => {
  const f = fixture();
  f.client.message.list = async () => ({ data: [{ type: "idle" }], cursor: { next: "more" } });
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.nodes[0].state, "unknown");
  assert.equal(result.truncated, true);
});

test("TTL refresh is shared and stale completed nodes become unknown until fresh evidence arrives", async () => {
  const f = fixture();
  await f.delegation.refresh("ses_root");
  assert.equal((await f.delegation.get("ses_root")).nodes[0].state, "completed");
  let release, started; let calls = 0;
  const listening = new Promise(resolve => { started = resolve; });
  f.client.session.list = async () => {
    calls++; await new Promise(resolve => { release = resolve; started(); });
    return { data: [], cursor: {} };
  };
  f.advance(3001);
  const stale = await f.delegation.get("ses_root");
  assert.equal(stale.nodes[0].state, "unknown");
  assert.equal(stale.stale, true);
  assert.match(stale.error, /stale/);
  await f.delegation.get("ses_root");
  const pending = f.delegation.refresh("ses_root");
  await listening;
  assert.equal(calls, 1);
  release(); await pending;
  assert.deepEqual((await f.delegation.get("ses_root")).nodes, []);
});

test("routine refresh keeps recently observed status until its freshness expires", async () => {
  const f = fixture();
  await f.delegation.refresh("ses_root");
  let release, started;
  const listening = new Promise(resolve => { started = resolve; });
  f.client.session.list = async () => {
    await new Promise(resolve => { release = resolve; started(); });
    return { data: [], cursor: {} };
  };
  f.advance(1001);
  const refreshing = await f.delegation.get("ses_root");
  assert.equal(refreshing.nodes[0].state, "completed");
  assert.equal(refreshing.error, null);
  await listening;
  const pending = f.delegation.refresh("ses_root");
  f.advance(2000);
  const stale = await f.delegation.get("ses_root");
  assert.equal(stale.nodes[0].state, "unknown");
  assert.match(stale.error, /stale/);
  release(); await pending;
});

test("refresh failure masks old completion and never exposes private exception content", async () => {
  const f = fixture();
  await f.delegation.refresh("ses_root");
  f.client.session.list = async () => { throw new Error("PRIVATE server payload"); };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.nodes[0].state, "unknown");
  assert.equal(result.stale, true);
  assert.match(result.error, /unavailable/);
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE/);
});

test("per-child failure leaves other status available and never falsely completes a failed read", async () => {
  const f = fixture([child("ses_a"), child("ses_b")]);
  f.facts.active.ses_a = { type: "running" };
  f.client.permission.list = async ({ sessionID }) => {
    if (sessionID === "ses_a") throw new Error("PRIVATE");
    return [];
  };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => node.state), ["unknown", "completed"]);
  assert.equal(result.stale, false);
  assert.match(result.error, /unavailable/);
});

test("failed descendant discovery preserves other branches and marks the tree incomplete", async () => {
  const f = fixture([child("ses_a"), child("ses_b"), child("ses_b1", "ses_b")]);
  const list = f.client.session.list;
  f.client.session.list = async input => {
    if (input.parentID === "ses_a") throw new Error("PRIVATE failure");
    return list(input);
  };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => node.id), ["ses_a", "ses_b", "ses_b1"]);
  assert.equal(result.truncated, true);
  assert.equal(result.stale, false);
  assert.ok(result.nodes.every(node => node.state === "completed"));
  assert.match(result.error, /hierarchy/);
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE/);
});

test("a failed later root page preserves observed workers without certifying removals", async () => {
  const f = fixture([child("ses_a"), child("ses_b")]);
  f.client.session.list = async input => {
    if (input.cursor) throw new Error("PRIVATE failure");
    if (input.parentID === "ses_root") return { data: [child("ses_a")], cursor: { next: "next" } };
    return { data: [], cursor: {} };
  };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => [node.id, node.state]), [["ses_a", "completed"]]);
  assert.equal(result.stale, false);
  assert.equal(result.truncated, true);
  assert.match(result.error, /hierarchy/);
});

test("failed refreshes are throttled, recover, and never leak another root", async () => {
  const f = fixture([child("ses_a"), child("ses_b", "ses_other")]);
  f.sessions.set("ses_other", { id: "ses_other" });
  await f.delegation.refresh("ses_root");
  await f.delegation.refresh("ses_other");
  const list = f.client.session.list;
  let failures = 0;
  f.client.session.list = async input => {
    if (input.parentID === "ses_root") { failures++; throw new Error(); }
    return list(input);
  };
  await f.delegation.refresh("ses_root");
  for (let i = 0; i < 3; i++) {
    const root = await f.delegation.get("ses_root");
    assert.equal(root.stale, true);
    assert.deepEqual(root.nodes.map(node => [node.id, node.state]), [["ses_a", "unknown"]]);
  }
  assert.equal(failures, 1);
  const other = await f.delegation.get("ses_other");
  assert.equal(other.stale, false);
  assert.deepEqual(other.nodes.map(node => [node.id, node.state]), [["ses_b", "completed"]]);
  f.client.session.list = list;
  f.advance(1000);
  await f.delegation.get("ses_root");
  await f.delegation.refresh("ses_root");
  assert.equal((await f.delegation.get("ses_root")).nodes[0].state, "completed");
});

test("an unavailable active-session list cannot falsely certify completion", async () => {
  const f = fixture([child("ses_a"), child("ses_b")]);
  f.facts.permissions.ses_b = [{ id: "pending" }];
  f.client.session.active = async () => { throw new Error("PRIVATE"); };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => node.state), ["unknown", "waiting"]);
  assert.match(result.error, /unavailable/);
});

test("newer idle failure and interruption override older completed assistant text", async () => {
  const f = fixture([child("ses_a"), child("ses_b")]);
  f.facts.messages.ses_a = [{ type: "agent-switched" }, { type: "idle", outcome: "failed" }, final()];
  f.facts.messages.ses_b = [{ type: "idle", outcome: "interrupted" }, final()];
  await f.delegation.refresh("ses_root");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.state), ["error", "idle"]);
});

test("network timeout returns unknown even when transport ignores cancellation", async () => {
  const f = fixture([], { timeout: 10, refreshTimeout: 30 });
  f.client.session.list = () => new Promise(() => {});
  await Promise.all([f.delegation.refresh("ses_root"), delay(40)]);
  assert.match((await f.delegation.get("ses_root")).error, /unavailable/);
});

test("ignored cancellation does not allow unbounded in-flight transport calls", async () => {
  const f = fixture([], { timeout: 10, refreshTimeout: 40 });
  let started = 0;
  f.client.session.get = () => { started++; return new Promise(() => {}); };
  await Promise.all([...Array(16)].map((_, i) => f.delegation.refresh(`ses_root${i}`)));
  await Promise.all([...Array(16)].map((_, i) => f.delegation.refresh(`ses_root${i}`)));
  assert.equal(started, 16);
  assert.equal(f.delegation.limiter.active, 16);
});

test("a nine-second refresh cannot rejuvenate its oldest evidence", async () => {
  const f = fixture();
  f.client.session.active = async () => { f.advance(9000); return {}; };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.stale, true);
  assert.equal(result.nodes[0].state, "unknown");
  assert.match(result.error, /stale/);
  await f.delegation.refresh("ses_root");
});

test("store read latency cannot return expired status as fresh", async () => {
  const f = fixture();
  await f.delegation.refresh("ses_root");
  f.delegation.store.list = async () => { f.advance(3000); return []; };
  const result = await f.delegation.get("ses_root");
  assert.equal(result.stale, true);
  assert.equal(result.nodes[0].state, "unknown");
});

test("64 workers stay fresh across refreshes at synthetic 30ms request latency", async t => {
  const nodes = Array.from({ length: 64 }, (_, i) => child(`ses_${String(i).padStart(3, "0")}`));
  const f = fixture(nodes, { now: Date.now });
  let active = 0, maximum = 0;
  const delayed = work => async (...args) => {
    maximum = Math.max(maximum, ++active);
    try { await delay(30); return await work(...args); } finally { active--; }
  };
  f.client.session.get = delayed(f.client.session.get);
  f.client.session.list = delayed(f.client.session.list);
  f.client.session.active = delayed(f.client.session.active);
  f.client.session.form.list = delayed(f.client.session.form.list);
  f.client.session.inbox.list = delayed(f.client.session.inbox.list);
  f.client.message.list = delayed(f.client.message.list);
  f.client.permission.list = delayed(f.client.permission.list);
  const started = Date.now();
  await f.delegation.refresh("ses_root");
  const duration = Date.now() - started;
  assert.ok(duration < 2000, `refresh took ${duration}ms`);
  for (let i = 0; i < 18; i++) {
    const result = await f.delegation.get("ses_root");
    assert.equal(result.stale, false, `poll ${i} after ${duration}ms initial refresh`);
    assert.equal(result.nodes.length, 64);
    assert.ok(result.nodes.every(node => node.state === "completed"));
    await delay(200);
  }
  await f.delegation.cache.get("ses_root").pending;
  assert.ok(maximum <= 16);
  assert.ok(maximum > 4);
  t.diagnostic(`initial refresh: ${duration}ms; peak concurrent requests: ${maximum}`);
});

test("real V2 client adapter uses only fake HTTP responses and verifies provenance", async () => {
  const calls = [];
  const sessions = { ses_root: { id: "ses_root" }, ses_a: child("ses_a") };
  const client = OpenCode.make({ baseUrl: "http://fake.invalid", fetch: async (url, init) => {
    const request = new URL(url); calls.push([init?.method || "GET", request.pathname, request.searchParams.get("parentID")]);
    assert.equal(request.origin, "http://fake.invalid");
    let data;
    if (request.pathname === "/api/session") return Response.json({ data: request.searchParams.get("parentID") === "ses_root" ? [sessions.ses_a] : [], cursor: {} });
    if (request.pathname === "/api/session/active") data = {};
    else if (request.pathname === "/api/session/ses_root") data = sessions.ses_root;
    else if (request.pathname === "/api/session/ses_a") data = sessions.ses_a;
    else if (request.pathname === "/api/session/ses_a/message/msg_final") data = final();
    else if (request.pathname === "/api/session/ses_a/message") return Response.json({ data: [final()], cursor: {} });
    else if (["/api/session/ses_a/permission", "/api/session/ses_a/form", "/api/session/ses_a/inbox"].includes(request.pathname)) data = [];
    else throw new Error(`Unexpected fake request ${request.pathname}`);
    return Response.json({ data });
  } });
  const delegation = new Delegation(client, { storePath: null });
  await delegation.refresh("ses_root");
  const result = await delegation.get("ses_root");
  assert.deepEqual(result.nodes.map(node => [node.id, node.state]), [["ses_a", "completed"]]);
  await delegation.publish("ses_root", card());
  assert.equal((await delegation.get("ses_root")).presentations[0].text, "Coordinator supplied");
  await delegation.acknowledge("ses_root", "ses_a");
  assert.deepEqual((await delegation.get("ses_root")).nodes, []);
  await delegation.restore("ses_root", "ses_a");
  assert.ok(calls.every(([method]) => method === "GET"));
  assert.doesNotMatch(JSON.stringify(await delegation.get("ses_root")), /PRIVATE/);
});

test("request concurrency and root cache are bounded", async () => {
  const f = fixture(Array.from({ length: 12 }, (_, i) => child(`ses_${i}`)));
  let active = 0; let maximum = 0;
  const list = f.client.session.list;
  f.client.session.list = async input => {
    maximum = Math.max(maximum, ++active);
    await delay(1); active--;
    return list(input);
  };
  await Promise.all(Array.from({ length: 20 }, (_, i) => f.delegation.get(`ses_root${i}`)));
  await Promise.all([...f.delegation.cache.values()].map(entry => entry.pending));
  assert.ok(f.delegation.cache.size <= 16);
  await f.delegation.refresh("ses_root");
  assert.ok(maximum <= 16);
});

test("titles are bounded by characters without truncating UTF-8 sequences", async () => {
  const f = fixture([child("ses_a", "ses_root", { title: "🙂".repeat(200) })]);
  await f.delegation.refresh("ses_root");
  assert.equal([...(await f.delegation.get("ses_root")).nodes[0].title].length, 160);
});

test("publication verifies actual ancestry and exact source message, but never copies message content", async () => {
  const f = fixture([child("ses_a"), child("ses_nested", "ses_a"), child("ses_other", "ses_else")]);
  const result = await f.delegation.publish("ses_root", card({ source_id: "ses_nested" }));
  assert.equal(result.text, "Coordinator supplied");
  assert.deepEqual(f.calls, [["get", "ses_nested"], ["get", "ses_a"], ["get", "ses_root"], ["message", "ses_nested", "msg_final"]]);
  await assert.rejects(f.delegation.publish("ses_root", card({ source_id: "ses_other" })), /provenance/);
  await assert.rejects(f.delegation.publish("ses_root", card({ source_message_id: "msg_missing" })), /provenance/);
  assert.equal((await f.delegation.store.list("ses_root")).length, 1);
  assert.doesNotMatch(JSON.stringify(await f.delegation.store.list("ses_root")), /PRIVATE/);
});

test("root and archived descendants can be sources, and cards survive descendant disappearance", async () => {
  const f = fixture([child("ses_a", "ses_root", { time: { archived: 1 } })]);
  await f.delegation.publish("ses_root", card());
  await f.delegation.publish("ses_root", card({ id: "card_root", source_id: "ses_root" }));
  f.sessions.delete("ses_a");
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.nodes.length, 0);
  assert.equal(result.presentations.length, 2);
});

test("unrelated and cyclic parent chains cannot publish", async () => {
  const f = fixture([child("ses_a", "ses_b"), child("ses_b", "ses_a"), { id: "ses_other" }]);
  await assert.rejects(f.delegation.publish("ses_root", card()), /not in this workspace/);
  await assert.rejects(f.delegation.publish("ses_root", card({ source_id: "ses_other" })), /not in this workspace/);
  assert.deepEqual(await f.delegation.store.list("ses_root"), []);
});

test("strict card validation enforces UTF-8 byte bounds, table dimensions, and ASCII references", () => {
  assert.equal(presentation(card({ text: "é".repeat(2048) })).text.length, 2048);
  assert.equal(presentation(card({ kind: "table", columns: ["Key"], rows: [["é".repeat(80)]] })).rows.length, 1);
  const invalid = [
    { text: "é".repeat(2049) }, { title: "t".repeat(161) }, { title: " " }, { id: "../../file" },
    { source_id: "ses_é" }, { source_message_id: "" }, { source_message_id: "not_a_message" },
    { extra: "unknown" }, { columns: ["not markdown"] }, { rows: null }, { kind: "html" },
    { kind: "table", columns: [] }, { kind: "table", columns: Array(7).fill("x") },
    { kind: "table", columns: ["é".repeat(41)] }, { kind: "table", columns: ["x"], rows: [["é".repeat(81)]] },
    { kind: "table", columns: ["x"], rows: Array(17).fill(["x"]) },
    { kind: "table", columns: ["x"], rows: [["x", "y"]] }, { text: null }, { columns: [4] },
  ];
  for (const update of invalid) assert.throws(() => presentation(card(update)), DelegationError);
});

test("explicit card IDs update idempotently; 16-card limit and removal are per root", async () => {
  const f = fixture();
  await Promise.all(Array.from({ length: 16 }, (_, i) => f.delegation.publish("ses_root", card({ id: `card_${i}` }))));
  await f.delegation.publish("ses_root", card({ id: "card_0", text: "Updated" }));
  assert.equal((await f.delegation.store.list("ses_root")).length, 16);
  assert.equal((await f.delegation.store.list("ses_root"))[0].text, "Updated");
  await assert.rejects(f.delegation.publish("ses_root", card({ id: "card_17" })), /limit/);
  assert.deepEqual(await f.delegation.remove("ses_root", "card_0"), { ok: true });
  await f.delegation.remove("ses_root", "card_0");
  assert.equal((await f.delegation.store.list("ses_root")).length, 15);
  await assert.rejects(f.delegation.publish("ses_root/escape", card()), /reference/);
  assert.throws(() => f.delegation.remove("ses_root", "../escape"), /reference/);
});

async function temporaryStore(t) {
  const directory = await mkdtemp("/tmp/opencode/workspace-test-");
  t.after(() => rm(directory, { recursive: true, force: true }));
  return join(directory, "presentations.json");
}

test("cards persist atomically with private file mode, including concurrent idempotent updates", async t => {
  const path = await temporaryStore(t);
  const f = fixture(undefined, { storePath: path });
  await Promise.all([f.delegation.publish("ses_root", card()), f.delegation.publish("ses_root", card({ text: "Latest" }))]);
  const store = new PresentationStore(path);
  assert.equal((await store.list("ses_root"))[0].text, "Latest");
  assert.equal((await stat(path)).mode & 0o777, 0o600);
  assert.equal(JSON.parse(await readFile(path, "utf8")).roots.length, 1);
  await store.remove("ses_root", "card_one");
  assert.deepEqual(await new PresentationStore(path).list("ses_root"), []);
});

test("corrupt store fails closed on reads, publish, and deletion, without overwriting it", async t => {
  const path = await temporaryStore(t);
  await writeFile(path, "CORRUPT PRIVATE STATE");
  const f = fixture(undefined, { storePath: path });
  await assert.rejects(f.delegation.publish("ses_root", card()), /corrupt/);
  await assert.rejects(f.delegation.remove("ses_root", "card_one"), /corrupt/);
  const result = await f.delegation.get("ses_root");
  assert.match(result.error, /corrupt/);
  assert.deepEqual(result.presentations, []);
  assert.equal(await readFile(path, "utf8"), "CORRUPT PRIVATE STATE");
  await f.delegation.refresh("ses_root");
  const fresh = await f.delegation.get("ses_root");
  assert.equal(fresh.stale, false);
  assert.equal(fresh.nodes[0].state, "completed");
  assert.match(fresh.error, /corrupt/);
});

test("structurally invalid persisted cards are not silently repaired or overwritten", async t => {
  const path = await temporaryStore(t);
  const invalid = JSON.stringify({ version: 1, roots: [{ id: "ses_root", presentations: [card({ text: "x".repeat(4097) })] }] });
  await writeFile(path, invalid);
  const store = new PresentationStore(path);
  await assert.rejects(store.list("ses_root"), /corrupt/);
  await assert.rejects(store.put("ses_root", card()), /corrupt/);
  assert.equal(await readFile(path, "utf8"), invalid);
});

test("a symlink store is rejected without touching its target", async t => {
  const path = await temporaryStore(t);
  const target = `${path}.target`;
  await writeFile(target, "PRIVATE TARGET");
  await symlink(target, path);
  const store = new PresentationStore(path);
  await assert.rejects(store.list("ses_root"), /corrupt/);
  await assert.rejects(store.put("ses_root", card()), /corrupt/);
  assert.equal(await readFile(target, "utf8"), "PRIVATE TARGET");
});

test("acknowledgement hides only the verified outcome and restore is idempotent", async () => {
  const f = fixture();
  assert.deepEqual(await f.delegation.acknowledge("ses_root", "ses_a"), { ok: true, worker_id: "ses_a", state: "completed" });
  assert.deepEqual(await f.delegation.acknowledge("ses_root", "ses_a"), { ok: true, worker_id: "ses_a", state: "completed" });
  assert.deepEqual((await f.delegation.get("ses_root")).nodes, []);
  assert.deepEqual(await f.delegation.acknowledged("ses_root"), [{ worker_id: "ses_a", state: "completed", superseded: false }]);
  assert.doesNotMatch(JSON.stringify(await f.delegation.acknowledged("ses_root")), /PRIVATE|msg_|digest/);
  assert.deepEqual(await f.delegation.restore("ses_root", "ses_a"), { ok: true });
  assert.deepEqual(await f.delegation.restore("ses_root", "ses_a"), { ok: true });
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.id), ["ses_a"]);
  assert.deepEqual(await f.delegation.acknowledged("ses_root"), []);
});

test("error acknowledgements require an identified failed event and detect changed outcomes", async () => {
  const f = fixture();
  f.facts.messages.ses_a = [{ id: "msg_idle1", type: "idle", outcome: "failed", error: { message: "PRIVATE" } }, final()];
  assert.equal((await f.delegation.acknowledge("ses_root", "ses_a")).state, "error");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes, []);
  f.facts.messages.ses_a = [{ id: "msg_idle2", type: "idle", outcome: "failed" }, final()];
  await f.delegation.refresh("ses_root");
  assert.equal((await f.delegation.get("ses_root")).nodes[0].state, "error");
  assert.equal((await f.delegation.acknowledged("ses_root"))[0].superseded, true);
  f.facts.messages.ses_a = [{ type: "idle", outcome: "failed" }, final()];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.facts.messages.ses_a = [final({ error: "failure", content: [{ type: "tool", state: { status: "running" } }] })];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.sessions.get("ses_a").outcome = "failed";
  f.facts.messages.ses_a = [final()];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.facts.messages.ses_a = [final({ error: "failure" })];
  assert.equal((await f.delegation.acknowledge("ses_root", "ses_a")).state, "error");
});

test("acknowledgement denies waiting, running, idle, unknown, incomplete and stale evidence", async () => {
  const f = fixture();
  f.facts.permissions.ses_a = [{ id: "perm" }];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  delete f.facts.permissions.ses_a;
  f.facts.forms.ses_a = [{ id: "question" }];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  delete f.facts.forms.ses_a;
  f.facts.active.ses_a = {};
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  delete f.facts.active.ses_a;
  f.facts.inbox.ses_a = [{}];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  delete f.facts.inbox.ses_a;
  f.facts.messages.ses_a = [final({ time: {} })];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.facts.messages.ses_a = [final({ id: undefined })];
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.facts.messages.ses_a = [final()];
  f.client.session.active = async () => { throw new Error("PRIVATE"); };
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.client.session.active = async () => ({});
  const permissions = f.client.permission.list;
  f.client.permission.list = async () => { throw new Error("PRIVATE"); };
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /verified terminal/);
  f.client.permission.list = permissions;
  f.client.session.active = async () => { f.advance(3000); return {}; };
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /Fresh/);
  assert.deepEqual(await f.delegation.acknowledged("ses_root"), []);
});

test("renewed activity and changed messages supersede acknowledgements, including after restart", async t => {
  const path = await temporaryStore(t);
  const f = fixture(undefined, { visibilityPath: path });
  await f.delegation.acknowledge("ses_root", "ses_a");
  await f.delegation.get("ses_root");
  f.facts.active.ses_a = {};
  await f.delegation.refresh("ses_root");
  assert.equal((await f.delegation.get("ses_root")).nodes[0].state, "running");
  delete f.facts.active.ses_a;
  await f.delegation.refresh("ses_root");
  assert.equal((await f.delegation.get("ses_root")).nodes[0].state, "completed");
  const restarted = fixture(undefined, { visibilityPath: path });
  await restarted.delegation.refresh("ses_root");
  assert.equal((await restarted.delegation.get("ses_root")).nodes[0].state, "completed");
  assert.equal((await restarted.delegation.acknowledged("ses_root"))[0].superseded, true);
  await restarted.delegation.acknowledge("ses_root", "ses_a");
  restarted.facts.messages.ses_a = [final({ content: [{ type: "text", text: "changed PRIVATE" }] })];
  await restarted.delegation.refresh("ses_root");
  assert.equal((await restarted.delegation.get("ses_root")).nodes[0].state, "completed");
  assert.equal((await restarted.delegation.acknowledged("ses_root"))[0].superseded, true);
  await restarted.delegation.acknowledge("ses_root", "ses_a");
  restarted.facts.messages.ses_a = [final({ id: "msg_new" })];
  await restarted.delegation.refresh("ses_root");
  assert.equal((await restarted.delegation.get("ses_root")).nodes[0].state, "completed");
  assert.doesNotMatch(JSON.stringify(await restarted.delegation.get("ses_root")), /PRIVATE|msg_new|digest/);
});

test("a failed supersession write is retried even if the worker has already settled again", async () => {
  const visibilityStore = new VisibilityStore(null);
  const f = fixture(undefined, { visibilityStore });
  await f.delegation.acknowledge("ses_root", "ses_a");
  const supersede = visibilityStore.supersede.bind(visibilityStore);
  let failures = 1;
  visibilityStore.supersede = (...args) => { if (failures > 0) { failures--; throw new Error("write error"); } return supersede(...args); };
  f.facts.active.ses_a = {};
  await f.delegation.refresh("ses_root");
  delete f.facts.active.ses_a;
  await f.delegation.refresh("ses_root");
  assert.equal((await f.delegation.get("ses_root")).nodes[0].state, "completed");
  assert.equal((await f.delegation.acknowledged("ses_root"))[0].superseded, true);
});

test("visible or undiscovered descendants retain their actual ancestors", async () => {
  const f = fixture([child("ses_a"), child("ses_b", "ses_a"), child("ses_c")]);
  await f.delegation.acknowledge("ses_root", "ses_a");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.id), ["ses_a", "ses_b", "ses_c"]);
  await f.delegation.acknowledge("ses_root", "ses_b");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.id), ["ses_c"]);
  f.facts.permissions.ses_b = [{ id: "pending" }];
  await f.delegation.refresh("ses_root");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => [node.id, node.parent_id, node.state]),
    [["ses_a", "ses_root", "completed"], ["ses_b", "ses_a", "waiting"], ["ses_c", "ses_root", "completed"]]);
  delete f.facts.permissions.ses_b;
  await f.delegation.acknowledge("ses_root", "ses_b");
  const list = f.client.session.list;
  f.client.session.list = async input => { if (input.parentID === "ses_a") throw new Error(); return list(input); };
  await f.delegation.refresh("ses_root");
  const partial = await f.delegation.get("ses_root");
  assert.equal(partial.truncated, true);
  assert.deepEqual(partial.nodes.map(node => [node.id, node.state]), [["ses_a", "completed"], ["ses_b", "unknown"], ["ses_c", "completed"]]);
  f.client.session.list = list;
  await f.delegation.refresh("ses_root");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.id), ["ses_c"]);
});

test("truncated unrelated branches allow individually verified leaf acknowledgements", async () => {
  const f = fixture([child("ses_a"), child("ses_b")]);
  const list = f.client.session.list;
  f.client.session.list = async input => { if (input.parentID === "ses_b") throw new Error(); return list(input); };
  await f.delegation.acknowledge("ses_root", "ses_a");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.truncated, true);
  assert.deepEqual(result.nodes.map(node => node.id), ["ses_b"]);
});

test("partial replacement preserves previously seen workers within the 64-node bound", async () => {
  const old = Array.from({ length: 64 }, (_, i) => child(`ses_old${String(i).padStart(2, "0")}`));
  const fresh = Array.from({ length: 64 }, (_, i) => child(`ses_new${String(i).padStart(2, "0")}`));
  const f = fixture(old);
  await f.delegation.refresh("ses_root");
  f.client.session.list = async input => {
    if (input.cursor) throw new Error("partial page");
    return { data: input.parentID === "ses_root" ? fresh : [], cursor: input.parentID === "ses_root" ? { next: "more" } : {} };
  };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.truncated, true);
  assert.equal(result.nodes.length, 64);
  assert.ok(result.nodes.every(node => node.id.startsWith("ses_old") && node.state === "unknown"));
});

test("partial replacement prioritizes active descendants and their real ancestors at the bound", async () => {
  const old = Array.from({ length: 64 }, (_, i) => child(`ses_old${String(i).padStart(2, "0")}`));
  const f = fixture(old);
  await f.delegation.refresh("ses_root");
  f.facts.permissions.ses_newchild = [{ id: "pending" }];
  f.client.session.list = async input => {
    if (input.cursor) throw new Error("partial page");
    if (input.parentID === "ses_root") return { data: [child("ses_newparent")], cursor: { next: "more" } };
    if (input.parentID === "ses_newparent") return { data: [child("ses_newchild", "ses_newparent")], cursor: {} };
    return { data: [], cursor: {} };
  };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.truncated, true);
  assert.equal(result.nodes.length, 64);
  assert.deepEqual(result.nodes.filter(node => node.id.startsWith("ses_new")).map(node => [node.id, node.parent_id, node.state]),
    [["ses_newparent", "ses_root", "completed"], ["ses_newchild", "ses_newparent", "waiting"]]);
});

test("stale cached visibility avoids flicker without claiming fresh status", async () => {
  const f = fixture([child("ses_a"), child("ses_b")]);
  await f.delegation.acknowledge("ses_root", "ses_a");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => node.id), ["ses_b"]);
  f.client.session.list = async () => { throw new Error(); };
  await f.delegation.refresh("ses_root");
  const result = await f.delegation.get("ses_root");
  assert.equal(result.stale, true);
  assert.deepEqual(result.nodes.map(node => [node.id, node.state]), [["ses_b", "unknown"]]);
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /Fresh/);
  await f.delegation.restore("ses_root", "ses_a");
  assert.deepEqual((await f.delegation.get("ses_root")).nodes.map(node => [node.id, node.state]), [["ses_a", "unknown"], ["ses_b", "unknown"]]);
});

test("acknowledgement checks the actual root and parent chain, not just list results", async () => {
  const f = fixture([child("ses_a"), child("ses_foreign", "ses_other"), { id: "ses_other" }]);
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_foreign"), /verified terminal/);
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_root"), /Invalid/);
  const get = f.client.session.get;
  f.client.session.get = async input => input.sessionID === "ses_a" ? child("ses_a", "ses_other") : get(input);
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /ancestry/);
  assert.deepEqual(await f.delegation.acknowledged("ses_root"), []);
  await f.delegation.restore("ses_other", "ses_a");
});

test("visibility store is private, bounded, persistent, and never overwrites corrupt or symlink state", async t => {
  const path = await temporaryStore(t);
  const f = fixture(undefined, { visibilityPath: path });
  await f.delegation.acknowledge("ses_root", "ses_a");
  assert.equal((await stat(path)).mode & 0o777, 0o600);
  const raw = await readFile(path, "utf8");
  assert.doesNotMatch(raw, /PRIVATE worker response|PRIVATE reasoning/);
  const store = new VisibilityStore(path);
  assert.equal((await store.list("ses_root")).length, 1);
  const record = (await store.list("ses_root"))[0];
  await chmod(path, 0o644);
  await assert.rejects(store.list("ses_root"), /corrupt/);
  await assert.rejects(store.put("ses_root", record), /corrupt/);
  assert.equal(await readFile(path, "utf8"), raw);
  await chmod(path, 0o600);
  await Promise.all(Array.from({ length: 63 }, (_, i) => store.put("ses_root", { ...record, id: `ses_x${i}` })));
  await assert.rejects(store.put("ses_root", { ...record, id: "ses_overflow" }), /64 per root/);
  await store.put("ses_root", record);
  assert.equal((await store.list("ses_root")).length, 64);
  await writeFile(path, "CORRUPT PRIVATE");
  await assert.rejects(store.list("ses_root"), /corrupt/);
  await assert.rejects(store.put("ses_root", record), /corrupt/);
  await assert.rejects(store.remove("ses_root", "ses_a"), /corrupt/);
  const projection = await f.delegation.get("ses_root");
  assert.match(projection.error, /visibility store/);
  assert.equal(projection.nodes[0].id, "ses_a");
  await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /corrupt/);
  assert.equal(await readFile(path, "utf8"), "CORRUPT PRIVATE");
  const target = `${path}.target`, link = `${path}.link`;
  await writeFile(target, "PRIVATE TARGET"); await symlink(target, link);
  const linked = new VisibilityStore(link);
  await assert.rejects(linked.put("ses_root", record), /corrupt/);
  assert.equal(await readFile(target, "utf8"), "PRIVATE TARGET");
});

test("visibility write errors are reported without creating a decision", async t => {
  const path = await temporaryStore(t);
  const directory = `${path}.dir`;
  await mkdir(directory); await chmod(directory, 0o500);
  try {
    if (process.getuid?.() === 0) { t.skip("root bypasses directory write permissions"); return; }
    const f = fixture(undefined, { visibilityPath: join(directory, "visibility.json") });
    await assert.rejects(f.delegation.acknowledge("ses_root", "ses_a"), /write failed/);
    assert.deepEqual(await f.delegation.acknowledged("ses_root"), []);
  } finally { await chmod(directory, 0o700); }
});

test("workspace CLI sends only explicit card operations to an existing fake loopback bridge", async t => {
  const f = fixture();
  await f.delegation.refresh("ses_root");
  const calls = [];
  const server = createServer(async (request, response) => {
    calls.push([request.method, request.url]);
    try {
      let result;
      if (request.method === "GET" && request.url === "/session/ses_root/delegation") result = await f.delegation.get("ses_root");
      else if (request.method === "GET" && request.url === "/session/ses_root/workers/acknowledged") result = await f.delegation.acknowledged("ses_root");
      else if (request.method === "POST" && request.url === "/session/ses_root/workers/ses_a/acknowledge") result = await f.delegation.acknowledge("ses_root", "ses_a");
      else if (request.method === "DELETE" && request.url === "/session/ses_root/workers/ses_a/acknowledge") result = await f.delegation.restore("ses_root", "ses_a");
      else if (request.method === "POST" && request.url === "/session/ses_root/presentations") {
        const chunks = []; for await (const chunk of request) chunks.push(chunk);
        result = await f.delegation.publish("ses_root", JSON.parse(Buffer.concat(chunks).toString()));
      } else if (request.method === "DELETE" && request.url === "/session/ses_root/presentations/card_one") {
        result = await f.delegation.remove("ses_root", "card_one");
      } else throw new Error("Unexpected route");
      response.writeHead(200, { "content-type": "application/json" }); response.end(JSON.stringify(result));
    } catch { response.writeHead(400, { "content-type": "application/json" }); response.end('{"error":"Fake failure"}'); }
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const run = (args, input = "") => new Promise((resolve, reject) => {
    const process = spawn(globalThis.process.execPath, ["scripts/workspace.mjs", ...args], {
      cwd: new URL("..", import.meta.url), env: { ...globalThis.process.env, TOMORI_BRIDGE_PORT: String(server.address().port) },
      stdio: ["pipe", "pipe", "pipe"], timeout: 5000,
    });
    let stdout = "", stderr = "";
    process.stdout.on("data", chunk => { stdout += chunk; });
    process.stderr.on("data", chunk => { stderr += chunk; });
    process.on("error", reject);
    process.on("close", code => resolve({ code, stdout, stderr }));
    process.stdin.end(input);
  });
  const published = await run(["present", "--session", "ses_root", "--json", "-"], JSON.stringify(card()));
  assert.equal(published.code, 0, published.stderr);
  assert.equal(JSON.parse(published.stdout).text, "Coordinator supplied");
  const table = await run(["present", "--session", "ses_root", "--source", "ses_a", "--message", "msg_final",
    "--title", "Table", "--id", "card_one", "--table", "-"], JSON.stringify({ columns: ["Key"], rows: [["Value"]] }));
  assert.equal(table.code, 0, table.stderr);
  assert.deepEqual(JSON.parse(table.stdout).rows, [["Value"]]);
  const listed = await run(["list", "--session", "ses_root"]);
  assert.equal(JSON.parse(listed.stdout).presentations.length, 1);
  assert.equal((await run(["remove", "--session", "ses_root", "--id", "card_one"])).code, 0);
  for (let i = 0; i < 2; i++) assert.equal((await run(["acknowledge", "--session", "ses_root", "--worker", "ses_a"])).code, 0);
  assert.deepEqual(JSON.parse((await run(["acknowledged", "--session", "ses_root"])).stdout),
    [{ worker_id: "ses_a", state: "completed", superseded: false }]);
  for (let i = 0; i < 2; i++) assert.equal((await run(["restore", "--session", "ses_root", "--worker", "ses_a"])).code, 0);
  assert.notEqual((await run(["acknowledge", "--session", "ses_root", "--worker", "../bad"])).code, 0);
  assert.notEqual((await run(["list", "--session", "ses_root", "--text", "ignored"])).code, 0);
  assert.notEqual((await run(["present", "--session", "ses_root", "--json", "-"], "x".repeat(65537))).code, 0);
  assert.deepEqual(calls, [["POST", "/session/ses_root/presentations"], ["POST", "/session/ses_root/presentations"],
    ["GET", "/session/ses_root/delegation"], ["DELETE", "/session/ses_root/presentations/card_one"],
    ["POST", "/session/ses_root/workers/ses_a/acknowledge"], ["POST", "/session/ses_root/workers/ses_a/acknowledge"],
    ["GET", "/session/ses_root/workers/acknowledged"], ["DELETE", "/session/ses_root/workers/ses_a/acknowledge"],
    ["DELETE", "/session/ses_root/workers/ses_a/acknowledge"]]);
});
