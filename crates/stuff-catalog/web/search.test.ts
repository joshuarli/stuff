import assert from "node:assert/strict";
import { test } from "node:test";
import { createPackageSearch, type PackageDocument } from "./search.ts";

const NAMES: [string, string, string][] = [
  ["serde", "cargo", "upstream"],
  ["serde_json", "cargo", "upstream"],
  ["h12tiny", "cargo", "native"],
  ["h12tiny-tls", "cargo", "native"],
  ["tokio", "cargo", "upstream"],
  ["@types/node", "npm", ""],
  ["typescript", "npm", ""],
  ["httpx", "pypi", ""],
];

const documents: PackageDocument[] = NAMES.map(([name, registry, source], id) => ({ id, name, registry, source }));
const search = createPackageSearch(documents);
const names = (query: string): string[] => (search(query) ?? []).map((id) => documents[id]!.name);

test("an empty query matches nothing in particular", () => {
  assert.equal(search(""), null);
  assert.equal(search("   "), null);
});

test("a prefix of any name segment matches", () => {
  assert.deepEqual(names("ser").sort(), ["serde", "serde_json"]);
  assert.deepEqual(names("tls"), ["h12tiny-tls"]);
  assert.deepEqual(names("types").sort(), ["@types/node", "typescript"]);
});

test("every word must match, so more words narrow the results", () => {
  assert.deepEqual(names("h12tiny tls"), ["h12tiny-tls"]);
  assert.deepEqual(names("types node"), ["@types/node"]);
  assert.deepEqual(names("serde pypi"), []);
});

test("case and accents do not matter", () => {
  assert.deepEqual(names("SERDE").sort(), ["serde", "serde_json"]);
  assert.deepEqual(names("tÿpescript"), ["typescript"]);
});

test("a typo in a longer word still finds the package", () => {
  assert.deepEqual(names("tokyo"), ["tokio"]);
  assert.deepEqual(names("typscript"), ["typescript"]);
});

test("registry and source are searchable", () => {
  assert.deepEqual(names("pypi"), ["httpx"]);
  assert.deepEqual(names("native").sort(), ["h12tiny", "h12tiny-tls"]);
});

test("a name match outranks a registry match", () => {
  const ranked = createPackageSearch([
    { id: 0, name: "left-pad", registry: "npm", source: "" },
    { id: 1, name: "npm", registry: "cargo", source: "" },
  ]);
  assert.deepEqual(ranked("npm"), [1, 0]);
});

test("a query with no matches is empty, not null", () => {
  assert.deepEqual(search("zzzzzz"), []);
});
