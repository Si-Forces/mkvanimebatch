// Byte-preserving adapters; matching and report redaction stay in home-path.sh.
import { spawn } from "node:child_process";
import { once } from "node:events";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

const checker = fileURLToPath(new URL("hooks/home-path.sh", import.meta.url));
const hex = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;
const env = { ...process.env, GIT_GRAFT_FILE: "/dev/null", LC_ALL: "C", GIT_LITERAL_PATHSPECS: "1" };
const flags = ["-U0", "--text", "--no-color", "--no-relative", "--no-ext-diff", "--no-textconv",
  "--src-prefix=a/", "--dst-prefix=b/", "-O/dev/null", "--ignore-submodules=none"];
const namesFlags = ["--no-color", "--no-relative", "--no-ext-diff", "--no-textconv", "--no-renames",
  "-O/dev/null", "--ignore-submodules=none"];

function command(program, args, options = {}) {
  const child = spawn(program, args, { env, stdio: ["ignore", "pipe", "ignore"], ...options });
  child.done = new Promise(resolve => {
    child.once("error", () => resolve(-1));
    child.once("close", code => resolve(code));
  });
  return child;
}
function git(args, options) { return command("git", ["--no-replace-objects", ...args], options); }
export async function write(stream, value) {
  if (!stream.write(value)) await once(stream, "drain");
}
export async function* records(stream, delimiter = 10) {
  let parts = [], length = 0;
  for await (const chunk of stream) {
    let start = 0, end;
    while ((end = chunk.indexOf(delimiter, start)) !== -1) {
      const part = chunk.subarray(start, end);
      yield Buffer.concat([...parts, part], length + part.length);
      parts = []; length = 0; start = end + 1;
    }
    if (start < chunk.length) { parts.push(chunk.subarray(start)); length += chunk.length - start; }
  }
  if (length) yield Buffer.concat(parts, length);
}
async function* output(child, delimiter = 10) {
  try {
    yield* records(child.stdout, delimiter);
    if (await child.done !== 0) throw new Error("cannot read Git input");
  } finally {
    // A completed membership query can stop before EOF; reap that reader too.
    child.kill();
    await child.done;
  }
}
async function small(args) {
  const child = git(args); let result = "";
  for await (const chunk of child.stdout) {
    result += chunk.toString("ascii");
    if (result.length > 16384) { child.kill(); throw new Error("oversized object header"); }
  }
  if (await child.done !== 0) throw new Error("cannot resolve Git object");
  return result.trim();
}
export function quote(bytes) {
  let count = bytes.length;
  if (bytes.includes(0)) { count = 0; for (const byte of bytes) if (byte) count++; }
  const value = Buffer.allocUnsafe(count * 4 + 2); let at = 0; value[at++] = 34;
  for (const byte of bytes) if (byte) {
    value[at++] = 92; value[at++] = 48 + (byte >> 6);
    value[at++] = 48 + ((byte >> 3) & 7); value[at++] = 48 + (byte & 7);
  }
  value[at] = 34;
  return value.toString("ascii");
}
async function emit(kind, id, number, name, text) {
  await write(process.stdout, `${kind} ${id} ${number} ${quote(name)} ${quote(text)}\n`);
}
async function name(kind, id, bytes) {
  const value = kind === "filename" ? Buffer.concat([Buffer.from("/"), bytes]) : bytes;
  await emit(kind, id, 0, bytes, value);
}

// cat-file --batch supplies exact stored bytes, without log-output encoding conversion.
class Objects {
  constructor() {
    this.child = git(["cat-file", "--batch"], { stdio: ["pipe", "pipe", "ignore"] });
    this.input = this.child.stdout[Symbol.asyncIterator](); this.buffer = Buffer.alloc(0);
    this.child.stdin.on("error", () => {});
  }
  async fill() {
    if (this.buffer.length) return;
    const next = await this.input.next();
    if (next.done) throw new Error("truncated object stream");
    this.buffer = next.value;
  }
  async line() {
    const parts = []; let size = 0;
    for (;;) {
      await this.fill(); const at = this.buffer.indexOf(10);
      if (at !== -1) {
        const part = this.buffer.subarray(0, at); this.buffer = this.buffer.subarray(at + 1);
        return Buffer.concat([...parts, part], size + part.length);
      }
      parts.push(this.buffer); size += this.buffer.length; this.buffer = Buffer.alloc(0);
    }
  }
  async* bytes(size) {
    while (size) {
      await this.fill(); const n = Math.min(size, this.buffer.length);
      const part = this.buffer.subarray(0, n); this.buffer = this.buffer.subarray(n); size -= n;
      yield part;
    }
  }
  async message(id, kind, report = true) {
    if (!hex.test(id)) throw new Error("invalid object id");
    await write(this.child.stdin, id + "\n");
    const header = (await this.line()).toString("ascii").split(" ");
    if (header[0] !== id || !["commit", "tag"].includes(header[1]) || !/^\d+$/.test(header[2]))
      throw new Error("invalid message object");
    const size = Number(header[2]);
    if (!Number.isSafeInteger(size)) throw new Error("invalid object size");
    let body = false, number = 1, headerNumber = 1, target;
    for await (const line of records(this.bytes(size))) {
      if (body) { if (report) await emit(kind, id, number++, Buffer.from(kind), line); }
      else if (!line.length) body = true;
      else {
        if (line.subarray(0, 7).equals(Buffer.from("object "))) {
          if (target) throw new Error("duplicate tag target");
          target = line.subarray(7).toString("ascii");
        }
        if (report) await emit(kind, id, headerNumber++, Buffer.from("header"), line);
      }
    }
    if ((await this.line()).length !== 0) throw new Error("invalid object separator");
    if (!body) throw new Error("missing message boundary");
    return target;
  }
  async close() {
    this.child.stdin.end();
    if (await this.child.done !== 0) throw new Error("object reader failed");
  }
  stop() { this.child.kill(); }
}

async function chain(id, objects) {
  let depth = 0;
  for (;;) {
    const type = await small(["cat-file", "-t", id]);
    if (type !== "tag") return { end: id, depth };
    id = await objects.message(id, "tag-message", false); depth++;
  }
}
async function commonTag(old, tip, objects) {
  if (/^0+$/.test(old)) return undefined;
  const left = await chain(old, objects), right = await chain(tip, objects);
  if (left.end !== right.end) return undefined;
  while (left.depth > right.depth) { old = await objects.message(old, "tag-message", false); left.depth--; }
  while (right.depth > left.depth) { tip = await objects.message(tip, "tag-message", false); right.depth--; }
  while (old !== tip) {
    old = await objects.message(old, "tag-message", false);
    tip = await objects.message(tip, "tag-message", false);
  }
  return tip;
}
async function peel(id, objects, boundary) {
  for (;;) {
    const type = await small(["cat-file", "-t", id]);
    if (type === "commit") return id;
    if (type !== "tag") throw new Error("published targets must resolve to commits");
    if (id === boundary) return small(["rev-parse", "--verify", `${id}^{commit}`]);
    id = await objects.message(id, "tag-message");
    if (!id || !hex.test(id)) throw new Error("invalid tag target");
  }
}

function gitPath(bytes) {
  if (bytes[0] !== 34) return bytes;
  if (bytes.at(-1) !== 34) throw new Error("invalid quoted path");
  const result = [], escapes = { 97: 7, 98: 8, 116: 9, 110: 10, 118: 11, 102: 12, 114: 13, 92: 92, 34: 34 };
  for (let i = 1; i < bytes.length - 1; i++) {
    let byte = bytes[i];
    if (byte === 92) {
      byte = bytes[++i];
      if (byte >= 48 && byte <= 51) {
        const octal = bytes.subarray(i, i + 3).toString("ascii");
        if (!/^[0-3][0-7][0-7]$/.test(octal) || octal === "000") throw new Error("invalid path escape");
        byte = parseInt(octal, 8); i += 2;
      } else if (Object.hasOwn(escapes, byte)) byte = escapes[byte];
      else throw new Error("invalid path escape");
    }
    result.push(byte);
  }
  return Buffer.from(result);
}
async function* mergePatches(id) {
  let entry;
  for await (const line of output(git(["diff-tree", "-r", "-c", "-p", "--no-commit-id", ...flags, "--no-renames", id]))) {
    const prefix = line.subarray(0, 10).toString("ascii");
    if (prefix === "diff --cc " || line.subarray(0, 16).toString("ascii") === "diff --combined ") {
      if (entry) yield entry;
      const offset = prefix === "diff --cc " ? 10 : 16;
      entry = { file: gitPath(line.subarray(offset)), binary: false };
    } else if (line.subarray(0, 13).toString("ascii") === "Binary files ") {
      if (!entry) throw new Error("missing merge file");
      entry.binary = true;
    }
  }
  if (entry) yield entry;
}

let probe;
async function matches(lines) {
  if (!probe) {
    const child = command("bash", [checker, "--probe"], { stdio: ["pipe", "pipe", "ignore"] });
    child.stdin.on("error", () => {});
    probe = { child, answers: records(child.stdout)[Symbol.asyncIterator]() };
  }
  await write(probe.child.stdin, lines.map(quote).join("\n") + "\n");
  const answers = [];
  for (let i = 0; i < lines.length; i++) {
    const answer = await probe.answers.next();
    if (answer.done || !/^[01]$/.test(answer.value.toString())) throw new Error("matcher failed");
    answers.push(answer.value[0] === 49);
  }
  return answers;
}
async function inherited(lines, parents, cache) {
  const answers = [], groups = new Map();
  let remaining = 0;
  for (const [i, line] of lines.entries()) {
    if (cache.line?.equals(line)) { answers[i] = cache.found; continue; }
    answers[i] = false;
    if (!groups.has(line.length)) groups.set(line.length, []);
    const group = groups.get(line.length);
    let needle = group.find(item => item.line.equals(line));
    if (!needle) { needle = { line, indexes: [], found: false }; group.push(needle); remaining++; }
    needle.indexes.push(i);
  }
  // Only this bounded candidate batch is indexed, never the parent-file lines.
  for (const parent of parents) {
    if (!remaining) break;
    if (/^0+$/.test(parent)) continue;
    for await (const other of output(git(["cat-file", "blob", parent]))) {
      for (const needle of groups.get(other.length) ?? [])
        if (!needle.found && other.equals(needle.line)) { needle.found = true; remaining--; }
      if (!remaining) break;
    }
  }
  for (const group of groups.values()) for (const needle of group)
    for (const i of needle.indexes) answers[i] = needle.found;
  if (lines.length) { cache.line = lines.at(-1); cache.found = answers.at(-1); }
  return answers;
}
async function content(id, file, old, current, parents) {
  if (/^0+$/.test(current)) return;
  let number = 1;
  let pending = [], bytes = 0;
  const cache = {};
  async function flush() {
    if (!pending.length) return;
    const hits = await matches(pending.map(item => item.line));
    const candidates = pending.filter((_, i) => hits[i]);
    const present = await inherited(candidates.map(item => item.line), parents, cache);
    for (let i = 0; i < candidates.length; i++) if (!present[i])
      await emit("binary", id, candidates[i].number, file, candidates[i].line);
    pending = []; bytes = 0;
  }
  async function candidate(line, at) {
    pending.push({ line, number: at }); bytes += line.length;
    if (pending.length >= 64 || bytes >= 65536) await flush();
  }
  if (/^0+$/.test(old)) {
    for await (const line of output(git(["cat-file", "blob", current]))) {
      await candidate(line, number);
      number++;
    }
    await flush(); return;
  }
  let inHunk = false;
  for await (const row of output(git(["diff", ...flags, old, current]))) {
    if (row.subarray(0, 2).toString() === "@@") {
      const hunk = row.toString("ascii").match(/^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
      if (!hunk) throw new Error("invalid binary hunk");
      number = Number(hunk[1]); inHunk = true;
    } else if (inHunk && row[0] === 43) {
      const line = row.subarray(1);
      await candidate(line, number);
      number++;
    } else if (inHunk && (row[0] === 32 || !row.length)) number++;
  }
  await flush();
}

async function changes(args, id = "0") {
  let commit = id, parents = [], pending, patches;
  async function drain() {
    if (patches && !(await patches.next()).done) throw new Error("unmatched merge file");
    patches = undefined;
  }
  for await (const token of output(git(args), 0)) {
    if (pending) {
      const { status, modes, blobs } = pending; pending = undefined;
      if (/^A+$/.test(status)) await name("filename", commit, token);
      if (patches) {
        const patch = await patches.next();
        if (patch.done || !patch.value.file.equals(token)) throw new Error("merge path mismatch");
        if (patch.value.binary && modes.at(-1) !== "160000" && !/^D+$/.test(status)) {
          const parentBlobs = blobs.slice(0, -1).map((blob, i) => modes[i] === "160000" ? "0" : blob);
          await content(commit, token, parentBlobs[0], blobs.at(-1), parentBlobs);
        }
      }
      continue;
    }
    const row = token.toString("ascii").replace(/^\n+/, "");
    if (!row) continue;
    if (row[0] === "@") {
      await drain();
      const ids = row.slice(1).trim().split(" ");
      if (!ids.every(value => hex.test(value))) throw new Error("invalid history header");
      [commit, ...parents] = ids;
      await objects.message(commit, "message");
      if (parents.length > 1) patches = mergePatches(commit)[Symbol.asyncIterator]();
    } else {
      const fields = row.match(/^(:+)([0-7]+(?: [0-7]+)*)(.*)$/);
      // Split by known parent count: modes, blob IDs, then status.
      const count = row.match(/^:+/)?.[0].length;
      if (!fields || !count) throw new Error("invalid raw change");
      const values = row.slice(count).split(" ");
      const modes = values.slice(0, count + 1), blobs = values.slice(count + 1, 2 * (count + 1));
      const status = values.at(-1);
      if (values.length !== 2 * (count + 1) + 1 || !modes.every(v => /^[0-7]{6}$/.test(v)) ||
          !blobs.every(v => hex.test(v)) || !/^[AMDT]+$/.test(status)) throw new Error("invalid raw fields");
      pending = { status, modes, blobs };
    }
  }
  if (pending) throw new Error("missing raw filename");
  await drain();
}
let objects;
async function main() {
  const [mode, ...args] = process.argv.slice(2);
  if (mode === "published" || mode === "messages") objects = new Objects();
  try {
    if (mode === "ref" && !args.length) {
      let count = 0;
      for await (const line of records(process.stdin)) { if (++count > 1) throw new Error("invalid ref input"); await name("ref", "0", line); }
      if (!count) throw new Error("empty ref input");
    } else if (mode === "messages" && !args.length) {
      for await (const line of records(process.stdin)) if (line.length)
        await objects.message(line.toString("ascii"), "message");
    } else if (mode === "staged" && !args.length) {
      await changes(["diff", "--cached", "--raw", "-z", "--no-abbrev", ...namesFlags]);
    } else if (mode === "between" && args.length === 2 && args.every(v => hex.test(v))) {
      await changes(["diff", "--raw", "-z", "--no-abbrev", ...namesFlags, ...args]);
    } else if (mode === "published" && args.length === 3 && args.every(v => v === "0" || hex.test(v))) {
      const [old, tip, baseline] = args;
      const commit = await peel(tip, objects, await commonTag(old, tip, objects));
      const exclude = baseline === "0" ? [] : ["--not", baseline];
      await changes(["log", "--no-show-signature", "--root", "-c", "--raw", "-z", "--no-abbrev", ...namesFlags,
        "--format=@%H %P", commit, ...exclude]);
    } else throw new Error("invalid adapter arguments");
    if (objects) await objects.close();
    if (probe) { probe.child.stdin.end(); if (await probe.child.done !== 0) throw new Error("matcher failed"); }
  } finally { if (objects) objects.stop(); if (probe) probe.child.kill(); }
}
function fail() { console.error("❌ [home-path] cannot inspect publication input safely."); process.exitCode = 1; }
try {
  if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url)))
    main().catch(fail);
} catch { fail(); }
