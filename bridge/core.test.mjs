import assert from "node:assert/strict";
import { test } from "node:test";
import { Bridge, InputError, answerValue, messageText } from "./core.mjs";
import { Delegation } from "./delegation.mjs";

function fixture() {
  const calls = [];
  const form = { id: "form_question", title: "Which option?", fields: [
    { key: "choice", type: "string", options: [{ label: "First", value: "first" }, { label: "Second", value: "second" }] },
    { key: "details", type: "string" },
  ] };
  const client = {
    session: {
      list: async input => { calls.push(["list", input]); return { data: [{ id: "ses_existing", title: "Existing work", location: { directory: "/workspace" } }] }; },
      active: async () => ({ ses_existing: { type: "running" } }),
      get: async ({ sessionID }) => ({ id: sessionID, title: "Existing work" }),
      prompt: async input => { calls.push(["prompt", input]); return { id: input.id }; },
      form: {
        list: async () => [form],
        reply: async input => { calls.push(["form", input]); },
      },
    },
    message: { list: async () => ({ data: [
      { id: "msg_a", type: "assistant", content: [{ type: "reasoning", text: "hidden reasoning" }, { type: "text", text: "Hello headset" }] },
      { id: "msg_u", type: "user", text: "Hello" },
    ] }) },
    permission: {
      list: async () => [{ id: "perm_1", action: "shell", resources: ["cargo test"] }],
      reply: async input => { calls.push(["permission", input]); },
    },
  };
  const delegation = { get: async () => ({ nodes: [], presentations: [], error: null, truncated: false }) };
  return { bridge: new Bridge(client, { delegation }), calls, form };
}

test("lists existing root sessions without creating or changing sessions", async () => {
  const { bridge, calls } = fixture();
  const list = await bridge.sessions();
  assert.equal(list[0].running, true);
  assert.equal(list[0].directory, "/workspace");
  assert.deepEqual(calls, [["list", { limit: 40, order: "desc", parentID: null, archived: false }]]);
});

test("projects readable history and pending questions, never hidden reasoning", async () => {
  const { bridge } = fixture();
  const snapshot = await bridge.snapshot("ses_existing");
  assert.deepEqual(snapshot.messages.map(message => message.text), ["Hello", "Hello headset"]);
  assert.equal(snapshot.approval.title, "shell");
  assert.match(snapshot.approval.text, /cargo test/);
  assert.equal(snapshot.form.field_key, "choice");
  assert.equal(messageText({ type: "assistant", content: [{ type: "reasoning", text: "secret" }] }), "");
});

test("speech projection includes only completed assistant text, not tool output or reasoning", async () => {
  const { bridge } = fixture();
  bridge.client.message.list = async () => ({ data: [
    { id: "msg_3", type: "assistant", time: { completed: 100 }, content: [{ type: "reasoning", text: "private" }, { type: "text", text: "Hello VR" }] },
    { id: "msg_2", type: "assistant", time: {}, content: [{ type: "text", text: "unfinished" }] },
    { id: "msg_1", type: "assistant", time: { completed: 100 }, content: [{ type: "tool", name: "shell", state: { type: "completed" } }] },
  ] });
  const messages = (await bridge.snapshot("ses_existing")).messages;
  assert.deepEqual(messages.map(m => [m.speech_text, m.completed]), [["", true], ["unfinished", false], ["Hello VR", true]]);
});

test("spoken message uses the selected session and stable id, with unchanged settings", async () => {
  const { bridge, calls } = fixture();
  await bridge.send("ses_existing", { id: "msg_vr_123", text: " hello " });
  assert.deepEqual(calls, [["prompt", { sessionID: "ses_existing", id: "msg_vr_123", text: "hello", delivery: "queue" }]]);
  await assert.rejects(bridge.send("ses_existing", { id: "bad", text: "hello" }), InputError);
});

test("permission replies are explicit once/reject, never always", async () => {
  const { bridge, calls } = fixture();
  await bridge.permission("ses_existing", "perm_1", { decision: "once" });
  await assert.rejects(bridge.permission("ses_existing", "perm_1", { decision: "always" }), InputError);
  assert.deepEqual(calls[0], ["permission", { sessionID: "ses_existing", requestID: "perm_1", decision: "once" }]);
});

test("multi-field voice answers are assembled before submitting the actual form", async () => {
  const { bridge, calls, form } = fixture();
  await bridge.answer("ses_existing", form.id, { key: "choice", text: "two." });
  assert.equal(calls.length, 0);
  assert.equal((await bridge.snapshot("ses_existing")).form.field_key, "details");
  await bridge.answer("ses_existing", form.id, { key: "details", text: "Some details" });
  assert.deepEqual(calls[0], ["form", { sessionID: "ses_existing", formID: form.id, answer: { choice: "second", details: "Some details" } }]);
});

test("invalid answers and stale fields do not trigger agent input", async () => {
  const { bridge, calls, form } = fixture();
  await assert.rejects(bridge.answer("ses_existing", form.id, { key: "choice", text: "nonsense" }), InputError);
  await assert.rejects(bridge.answer("ses_existing", form.id, { key: "wrong", text: "one" }), InputError);
  assert.equal(calls.length, 0);
  assert.equal(answerValue({ type: "boolean" }, "Yes."), true);
  assert.equal(answerValue({ type: "integer" }, "three"), 3);
  assert.throws(() => answerValue({ type: "boolean" }, "perhaps"), InputError);
});

test("delegation errors are isolated from the selected session snapshot", async () => {
  const { bridge } = fixture();
  bridge.delegation = { get: async () => { throw new Error("private failure detail"); } };
  const snapshot = await bridge.snapshot("ses_existing");
  assert.equal(snapshot.title, "Existing work");
  assert.equal(snapshot.messages.length, 2);
  assert.deepEqual(snapshot.delegation, {
    nodes: [], presentations: [], error: "Delegation unavailable.", truncated: false, stale: true,
  });
});

test("a stalled descendant refresh never holds up core chat", async () => {
  const { bridge } = fixture();
  let release, started;
  const listening = new Promise(resolve => { started = resolve; });
  bridge.client.session.list = () => new Promise(resolve => { release = resolve; started(); });
  bridge.delegation = new Delegation(bridge.client, { storePath: null });
  const snapshot = await bridge.snapshot("ses_existing");
  assert.equal(snapshot.messages.length, 2);
  assert.match(snapshot.delegation.error, /refreshing/);
  await listening;
  release({ data: [], cursor: {} });
  await bridge.delegation.refresh("ses_existing");
});

test("even a stalled delegation read is bounded independently of core chat", async () => {
  const { bridge } = fixture();
  bridge.delegation = { get: () => new Promise(() => {}) };
  const snapshot = await bridge.snapshot("ses_existing");
  assert.equal(snapshot.messages.length, 2);
  assert.match(snapshot.delegation.error, /unavailable/);
});
