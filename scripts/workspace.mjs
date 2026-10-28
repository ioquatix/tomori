#!/usr/bin/env node
import { createReadStream } from "node:fs";

const usage = `Coordinator workspace cards and worker visibility (existing loopback bridge only):
  node scripts/workspace.mjs list --session ROOT
  node scripts/workspace.mjs acknowledge --session ROOT --worker CHILD
  node scripts/workspace.mjs restore --session ROOT --worker CHILD
  node scripts/workspace.mjs acknowledged --session ROOT
  node scripts/workspace.mjs present --session ROOT --source CHILD --message MSG --title TITLE --text TEXT [--id CARD]
  node scripts/workspace.mjs present --session ROOT --source CHILD --message MSG --title TITLE --table FILE [--id CARD]
  node scripts/workspace.mjs remove --session ROOT --id CARD
--table - reads JSON from stdin; table JSON: {"columns":["Name"],"rows":[["Value"]]}.
--json FILE (or -) instead reads the complete presentation object.
Set BASE_SWARM_BRIDGE_PORT to select the local bridge (default 8765).
Publication is explicit; supply only reviewed content, never secrets or hidden reasoning.
Acknowledgement hides only the verified outcome, not the session; visible descendants keep ancestors visible.
Records remain until restored (maximum 64 per root); superseded records no longer hide workers.`;

async function boundedText(stream) {
  const chunks = []; let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (size > 65536) throw new Error("Input too large.");
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}

async function jsonFile(path) {
  const text = await boundedText(path === "-" ? process.stdin : createReadStream(path, { highWaterMark: 16384 }));
  try { return JSON.parse(text); } catch { throw new Error("Invalid input JSON."); }
}

async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (!command || ["help", "--help", "-h"].includes(command)) { console.log(usage); return; }
  if (!["list", "present", "remove", "acknowledge", "restore", "acknowledged"].includes(command)) throw new Error("Unknown command; use --help.");
  const options = {};
  for (let i = 0; i < args.length; i += 2) {
    if (!["--session", "--source", "--message", "--title", "--text", "--id", "--table", "--json", "--worker"].includes(args[i]) ||
        args[i + 1] === undefined || Object.hasOwn(options, args[i])) throw new Error("Invalid arguments; use --help.");
    options[args[i]] = args[i + 1];
  }
  if (!/^ses[A-Za-z0-9_-]{1,125}$/.test(options["--session"] || "")) throw new Error("A valid --session ROOT is required.");
  const allowed = ["list", "acknowledged"].includes(command) ? ["--session"] :
    ["acknowledge", "restore"].includes(command) ? ["--session", "--worker"] : command === "remove" ? ["--session", "--id"] :
    ["--session", "--source", "--message", "--title", "--text", "--id", "--table", "--json"];
  if (Object.keys(options).some(key => !allowed.includes(key))) throw new Error("Invalid arguments; use --help.");
  const port = Number(process.env.BASE_SWARM_BRIDGE_PORT || 8765);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid bridge port.");
  let path = `/session/${options["--session"]}`;
  const request = { method: "GET", signal: AbortSignal.timeout(command === "acknowledge" ? 35000 : 15000), redirect: "error" };
  if (command === "list") path += "/delegation";
  if (command === "acknowledged") path += "/workers/acknowledged";
  if (["acknowledge", "restore"].includes(command)) {
    if (!/^ses[A-Za-z0-9_-]{1,125}$/.test(options["--worker"] || "")) throw new Error("A valid --worker CHILD is required.");
    path += `/workers/${options["--worker"]}/acknowledge`;
    request.method = command === "acknowledge" ? "POST" : "DELETE";
  }
  if (command === "remove") {
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(options["--id"] || "")) throw new Error("A valid --id CARD is required.");
    path += `/presentations/${options["--id"]}`; request.method = "DELETE";
  }
  if (command === "present") {
    if (options["--json"] && Object.keys(options).some(key => !["--json", "--session"].includes(key))) {
      throw new Error("--json cannot be combined with presentation fields.");
    }
    let card;
    if (options["--json"]) card = await jsonFile(options["--json"]);
    else {
      card = { source_id: options["--source"], source_message_id: options["--message"],
        title: options["--title"], kind: "markdown", text: options["--text"] || "" };
      if (options["--id"]) card.id = options["--id"];
      if (options["--table"]) {
        const table = await jsonFile(options["--table"]);
        if (!table || typeof table !== "object" || Array.isArray(table) ||
            Object.keys(table).some(key => !["columns", "rows"].includes(key))) throw new Error("Invalid table JSON.");
        card = { ...card, kind: "table", columns: table.columns, rows: table.rows };
      }
    }
    path += "/presentations"; request.method = "POST";
    request.headers = { "content-type": "application/json" }; request.body = JSON.stringify(card);
  }
  const response = await fetch(`http://127.0.0.1:${port}${path}`, request);
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || `Bridge request failed (${response.status}).`);
  console.log(JSON.stringify(result, null, 2));
}

main().catch(error => { console.error(error.message || "Workspace request failed."); process.exitCode = 1; });
