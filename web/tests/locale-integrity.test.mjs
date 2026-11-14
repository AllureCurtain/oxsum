// Locale catalogue integrity (issue #1215): every `src/locales/*.json` must
// parse, carry no duplicate key, and agree with every other locale on its key
// set, value types and interpolation placeholders. `JSON.parse` silently keeps
// the last of two duplicate keys and a missing translation silently falls back
// to the key, so neither the build nor the type check would notice; a syntax
// error otherwise surfaces only minutes later inside the Docker frontend stage
// (11-11: two locale edits merged into invalid JSON and main's frontend went
// red eleven times). Dependency-free, so it runs before `npm ci` too.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

const directory = new URL("../src/locales/", import.meta.url);
const files = readdirSync(directory).filter((name) => name.endsWith(".json")).sort();

/** Reports every object key that appears twice inside the same object, with its path and line. */
export function duplicateKeys(text) {
  const duplicates = [];
  const stack = []; // per open container: { keys: Set|null, path, pendingKey }
  let line = 1;
  let i = 0;
  const path = () => stack.map((frame) => frame.label).filter(Boolean).join(".");
  while (i < text.length) {
    const c = text[i];
    if (c === "\n") line += 1;
    if (c === '"') {
      let j = i + 1;
      let value = "";
      while (text[j] !== '"') {
        if (text[j] === "\\") {
          value += text.slice(j, j + 2);
          j += 2;
          continue;
        }
        if (j >= text.length) throw new SyntaxError(`unterminated string at line ${line}`);
        value += text[j];
        j += 1;
      }
      const frame = stack[stack.length - 1];
      let k = j + 1;
      while (/\s/.test(text[k] ?? "")) k += 1;
      if (frame?.keys && text[k] === ":") {
        if (frame.keys.has(value)) duplicates.push(`${[path(), value].filter(Boolean).join(".")} (line ${line})`);
        frame.keys.add(value);
        frame.pendingKey = value;
      }
      i = j + 1;
      continue;
    }
    if (c === "{" || c === "[") {
      const parent = stack[stack.length - 1];
      stack.push({ keys: c === "{" ? new Set() : null, label: parent?.pendingKey ?? "", pendingKey: null });
      if (parent) parent.pendingKey = null;
    } else if (c === "}" || c === "]") {
      stack.pop();
    }
    i += 1;
  }
  return duplicates;
}

/** `{ "a.b": value }` for every leaf. */
function leaves(value, prefix = "", out = {}) {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    for (const [key, child] of Object.entries(value)) leaves(child, prefix ? `${prefix}.${key}` : key, out);
  } else {
    out[prefix] = value;
  }
  return out;
}

const placeholders = (text) => [...String(text).matchAll(/\{\{\s*([^}\s,]+)[^}]*\}\}/g)].map((m) => m[1]).sort();

const catalogues = Object.fromEntries(
  files.map((name) => [name, readFileSync(new URL(name, directory), "utf8")]),
);

test("there is more than one locale to compare", () => {
  assert.ok(files.length >= 2, `expected several locales, found ${files.join(", ")}`);
});

for (const name of files) {
  test(`${name} is valid JSON`, () => {
    assert.doesNotThrow(() => JSON.parse(catalogues[name]), `${name} does not parse`);
  });
  test(`${name} has no duplicate keys`, () => {
    assert.deepEqual(duplicateKeys(catalogues[name]), [], `${name} repeats keys; the first value is silently lost`);
  });
}

test("every locale carries the same keys, value types and placeholders", () => {
  const parsed = Object.fromEntries(files.map((name) => [name, leaves(JSON.parse(catalogues[name]))]));
  const [reference, ...others] = files;
  const problems = [];
  for (const other of others) {
    const a = parsed[reference];
    const b = parsed[other];
    for (const key of Object.keys(a)) if (!(key in b)) problems.push(`${other} lacks ${key}`);
    for (const key of Object.keys(b)) if (!(key in a)) problems.push(`${reference} lacks ${key}`);
    for (const key of Object.keys(a)) {
      if (!(key in b)) continue;
      if (typeof a[key] !== typeof b[key]) problems.push(`${key}: ${typeof a[key]} in ${reference}, ${typeof b[key]} in ${other}`);
      else if (placeholders(a[key]).join() !== placeholders(b[key]).join()) {
        problems.push(`${key}: placeholders {${placeholders(a[key])}} in ${reference}, {${placeholders(b[key])}} in ${other}`);
      }
    }
  }
  assert.deepEqual(problems, []);
});

test("the duplicate-key scanner sees nested and top-level repeats", () => {
  assert.deepEqual(duplicateKeys('{"a": 1, "b": {"c": "x\\"y", "c": 2}, "a": 3}'), ["b.c (line 1)", "a (line 1)"]);
  assert.deepEqual(duplicateKeys('{"a": {"b": 1}, "c": {"b": 2}, "d": ["b", "b"]}'), []);
});
