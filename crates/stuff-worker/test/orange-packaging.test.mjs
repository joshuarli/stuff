import assert from "node:assert/strict";
import test from "node:test";
import { mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { stage } from "../scripts/stage-orange.mjs";

const scratch = resolve(import.meta.dirname, "../../../.work/tmp");

test("staged Orange descriptor and worker modules survive relocation", (t) => {
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(join(scratch, "orange-package-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const original = join(dir, "original");
  const relocated = join(dir, "relocated");
  stage(original, "packaging-test");
  renameSync(original, relocated);
  const config = JSON.parse(readFileSync(join(relocated, "stuff.jsonc"), "utf8"));
  assert.equal(config.vars.STUFF_RELEASE, "packaging-test");
  assert.equal(isAbsolute(config.main), false);
  const main = resolve(relocated, config.main);
  assert.ok(!relative(relocated, main).startsWith(".."));
  const entry = readFileSync(main, "utf8");
  assert.match(entry, /build\/wasm\/stuff_worker\.js/);
  const glue = readFileSync(join(dirname(main), "build/wasm/stuff_worker.js"), "utf8");
  assert.match(glue, /stuff_worker_bg\.wasm/);
  assert.deepEqual([...readFileSync(join(dirname(main), "build/wasm/stuff_worker_bg.wasm")).subarray(0, 4)], [0, 97, 115, 109]);
});
