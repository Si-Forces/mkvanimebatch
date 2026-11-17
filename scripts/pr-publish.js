// Publish only the prepared description that passed the canonical home-path check.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { records } from "./home-path-input.js";

const checker = fileURLToPath(new URL("hooks/home-path.sh", import.meta.url));
let temporary, active;
process.once("exit", () => { if (temporary) fs.rmSync(temporary, { recursive: true, force: true }); });
for (const [signal, status] of [["SIGINT", 130], ["SIGTERM", 143]])
  process.once(signal, () => { active?.kill(signal); process.exit(status); });

function launch(program, args, options = {}) {
  const child = spawn(program, args, { stdio: ["pipe", "pipe", "pipe"], ...options });
  child.done = new Promise(resolve => {
    child.once("error", () => resolve(-1)); child.once("close", code => resolve(code));
  });
  active = child;
  return child;
}

async function main() {
  const [operation, ...args] = process.argv.slice(2);
  if (!["create", "edit"].includes(operation)) throw new Error("invalid operation");
  let pr;
  if (operation === "edit") {
    pr = args.shift();
    if (!pr || !/^[1-9][0-9]*$/.test(pr)) throw new Error("PR number required");
  }
  const options = new Map();
  for (let i = 0; i < args.length; i++) {
    const flag = args[i];
    if (operation === "create" && flag === "--draft" && !options.has(flag)) { options.set(flag, true); continue; }
    const allowed = operation === "create" ? ["--title", "--head", "--base", "--repo", "--body-file"] : ["--repo", "--body-file"];
    if (!allowed.includes(flag) || options.has(flag) || args[i + 1] === undefined) throw new Error("invalid option");
    options.set(flag, args[++i]);
  }
  if (!options.has("--body-file") || (operation === "create" &&
      ["--title", "--head"].some(flag => !options.get(flag)?.trim()))) throw new Error("prepared input required");

  temporary = fs.mkdtempSync(path.join(os.tmpdir(), "mkv-pr-"));
  const snapshot = path.join(temporary, "body");
  const source = options.get("--body-file");
  await pipeline(source === "-" ? process.stdin : fs.createReadStream(source),
    fs.createWriteStream(snapshot, { flags: "wx", mode: 0o600 }));

  const scan = launch("bash", [checker, "--text", "pr-body"]);
  scan.stdout.resume();
  const scanned = pipeline(fs.createReadStream(snapshot), scan.stdin).catch(() => false);
  let reported = false;
  for await (const line of records(scan.stderr)) {
    const location = line.toString("ascii").match(/^     pr-body:[0-9]+$/);
    if (location) {
      if (!reported) console.error("❌ [home-path] PR description names your home directory:");
      reported = true; console.error(location[0]);
    } else if (line.includes(Buffer.from("HOME and USERPROFILE name no home directory")))
      console.error("⚠ [home-path] no qualifying home identity; no PR body identities are checked.");
  }
  const scanStatus = await scan.done;
  const inputStatus = await scanned;
  if (scanStatus !== 0 || inputStatus === false) throw new Error("body validation failed");

  const forwarded = ["pr", operation];
  if (pr) forwarded.push(pr);
  for (const [flag, value] of options) if (flag !== "--body-file") {
    forwarded.push(flag); if (value !== true) forwarded.push(value);
  }
  forwarded.push("--body-file", "-");
  const publish = launch("gh", forwarded, {
    env: { ...process.env, GH_PROMPT_DISABLED: "1", GIT_TERMINAL_PROMPT: "0" },
    stdio: ["pipe", "pipe", "ignore"],
  });
  const sent = pipeline(fs.createReadStream(snapshot), publish.stdin).catch(() => false);
  let response = "";
  for await (const chunk of publish.stdout) {
    response += chunk.toString("utf8");
    if (response.length > 16384) { publish.kill(); throw new Error("invalid publication response"); }
  }
  const publishStatus = await publish.done;
  const sendStatus = await sent;
  if (publishStatus !== 0 || sendStatus === false) throw new Error("publication failed");
  const url = response.trim();
  if (/^https:\/\/[^\s]+\/pull\/[1-9][0-9]*\/?$/.test(url)) console.log(url);
  else console.log("PR description published.");
}
main().catch(() => {
  console.error("❌ [pr-publish] refused or failed; no unsafe input or command output is displayed.");
  process.exitCode = 1;
});
