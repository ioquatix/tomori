// Small projection of the OpenCode V2 API for a headset, not a second agent.
import { Delegation } from "./delegation.mjs";

export class InputError extends Error {}

export function messageText(message) {
  if (message.type === "user") return message.text || "";
  if (message.type !== "assistant") return "";
  const text = message.content.filter(part => part.type === "text").map(part => part.text).join("\n\n");
  if (text) return text;
  if (message.error) return `Agent error: ${message.error.message || JSON.stringify(message.error)}`;
  return message.content.filter(part => part.type === "tool")
    .map(part => `[${part.name}: ${part.state.type || part.state.status || "working"}]`).join("\n");
}

const normal = text => String(text).toLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();
const numbers = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

function optionValue(field, text) {
  const input = normal(text);
  const exact = (field.options || []).filter(option => normal(option.label) === input || normal(option.value) === input);
  if (exact.length === 1) return exact[0].value;
  const index = /^\d+$/.test(input) ? Number(input) : numbers.indexOf(input);
  if (index > 0 && index <= (field.options || []).length) return field.options[index - 1].value;
  if (field.custom) return text.trim();
  throw new InputError("Say an option's label or number. No answer was submitted.");
}

export function answerValue(field, text) {
  if (field.type === "string") return field.options?.length ? optionValue(field, text) : text.trim();
  if (field.type === "boolean") {
    const input = normal(text);
    if (["yes", "true"].includes(input)) return true;
    if (["no", "false"].includes(input)) return false;
    throw new InputError("Please say yes or no.");
  }
  if (field.type === "number" || field.type === "integer") {
    const input = normal(text);
    const word = numbers.indexOf(input);
    const numeric = text.trim().replace(/[.!?]$/, "");
    const value = word >= 0 ? word : /^-?\d+(?:\.\d+)?$/.test(numeric) ? Number(numeric) : NaN;
    if (!Number.isFinite(value) || (field.type === "integer" && !Number.isInteger(value))) {
      throw new InputError("Please say a number.");
    }
    return value;
  }
  if (field.type === "multiselect") {
    return [...new Set(text.replace(/[.!?]$/, "").split(/,|\band\b/i).map(part => optionValue(field, part)))];
  }
  throw new InputError("Complete this question in desktop OpenCode.");
}

export class Bridge {
  constructor(client, { delegation = new Delegation(client) } = {}) {
    this.client = client;
    this.delegation = delegation;
    this.drafts = new Map();
  }

  options() { return { signal: AbortSignal.timeout(8000) }; }

  async sessions() {
    const [list, active] = await Promise.all([
      this.client.session.list({ limit: 40, order: "desc", parentID: null, archived: false }, this.options()),
      this.client.session.active(this.options()),
    ]);
    return list.data.map(session => ({
      id: session.id, title: session.title || session.id,
      directory: session.location.directory, running: !!active[session.id],
    }));
  }

  field(form, sessionID) {
    const fields = form.fields.filter(field => !field.hidden);
    const supported = fields.length > 0 && fields.every(field =>
      ["string", "boolean", "number", "integer", "multiselect"].includes(field.type) && !field.when?.length);
    const answer = this.drafts.get(`${sessionID}/${form.id}`) || {};
    const field = fields.find(field => !Object.hasOwn(answer, field.key));
    return { fields, answer, field, supported };
  }

  formPrompt(form, sessionID) {
    const { field, fields, answer, supported } = this.field(form, sessionID);
    const options = field?.options?.map((option, i) => `${i + 1}. ${option.label}${option.description ? `: ${option.description}` : ""}`).join("\n");
    return {
      id: form.id, title: form.title, field_key: field?.key || "", supported,
      text: !supported ? "Complete this question in desktop OpenCode. Conditional/external forms are not supported in this demo."
        : `${Object.keys(answer).length + 1}/${fields.length}: ${field?.title || field?.key || "Answer"}\n${field?.description || ""}\n${options || ""}\nHold X and say your answer${options ? " or its number" : ""}.`,
    };
  }

  async snapshot(sessionID) {
    // Descendant discovery never delays core chat or takes it down. Its own
    // cache exposes stale/failed evidence explicitly, without worker transcripts.
    const fallback = () => ({ nodes: [], presentations: [], error: "Delegation unavailable.", truncated: false, stale: true });
    const delegation = new Promise(resolve => {
      const timer = setTimeout(() => resolve(fallback()), 250);
      Promise.resolve().then(() => this.delegation.get(sessionID)).then(resolve, () => resolve(fallback()))
        .finally(() => clearTimeout(timer));
    });
    const [session, messages, permissions, forms, active] = await Promise.all([
      this.client.session.get({ sessionID }, this.options()),
      this.client.message.list({ sessionID, limit: 24, order: "desc" }, this.options()),
      this.client.permission.list({ sessionID }, this.options()),
      this.client.session.form.list({ sessionID }, this.options()),
      this.client.session.active(this.options()),
    ]);
    const permission = permissions[0];
    return {
      id: sessionID, title: session.title || sessionID, running: !!active[sessionID],
      messages: messages.data.slice().reverse().map(message => ({
        id: message.id, role: message.type, text: messageText(message).slice(0, 20000),
        speech_text: message.type === "assistant" ? message.content.filter(part => part.type === "text").map(part => part.text).join("\n\n").slice(0, 20000) : "",
        completed: message.type === "assistant" && !!message.time?.completed,
      })).filter(message => message.text),
      approval: permission ? {
        id: permission.id, title: permission.action,
        text: `${permission.message || ""}\n${permission.resources.join("\n")}\n\nA: allow once. B: deny.`,
      } : null,
      form: forms[0] ? this.formPrompt(forms[0], sessionID) : null,
      delegation: await delegation,
    };
  }

  async send(sessionID, body) {
    if (typeof body.text !== "string" || !body.text.trim() || body.text.length > 500000 || !/^msg_vr_[a-zA-Z0-9_]+$/.test(body.id || "")) {
      throw new InputError("Invalid voice message.");
    }
    // Do not change the session's model, agent, tools, location, or permissions.
    const message = await this.client.session.prompt({
      sessionID, id: body.id, text: body.text.trim(), delivery: "queue",
    }, this.options());
    return { ok: true, id: message.id, note: "Message sent (queued if agent is busy)." };
  }

  async permission(sessionID, requestID, body) {
    if (!["once", "reject"].includes(body.decision)) throw new InputError("Choose allow once or deny.");
    await this.client.permission.reply({ sessionID, requestID, decision: body.decision }, this.options());
    return { ok: true, note: body.decision === "once" ? "Allowed once." : "Denied." };
  }

  async answer(sessionID, formID, body) {
    const forms = await this.client.session.form.list({ sessionID }, this.options());
    const form = forms.find(form => form.id === formID);
    if (!form) throw new InputError("This question is no longer pending.");
    const { field, fields, answer, supported } = this.field(form, sessionID);
    if (!supported || !field || body.key !== field.key) throw new InputError("Question changed; read the current panel and retry.");
    if (typeof body.text !== "string" || !body.text.trim() || body.text.length > 500000) throw new InputError("Empty or oversized answer.");
    const next = { ...answer, [field.key]: answerValue(field, body.text) };
    if (fields.every(field => Object.hasOwn(next, field.key))) {
      await this.client.session.form.reply({ sessionID, formID, answer: next }, this.options());
      this.drafts.delete(`${sessionID}/${formID}`);
      return { ok: true, note: "Answer submitted." };
    }
    this.drafts.set(`${sessionID}/${formID}`, next);
    return { ok: true, note: "Answer saved. Read the next question." };
  }
}
