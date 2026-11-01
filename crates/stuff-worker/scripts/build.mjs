#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { loadAndParseConfig, registerConfigHooks } from "@cloudflare/config";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const bin = (name) => join(root, "node_modules/.bin", name);
const outputRoot = join(root, ".cloudflare/output/v0");
const workerDir = join(outputRoot, "workers/default");
const bundleDir = join(workerDir, "bundle");
const buildContext = { isPreview: false, mode: "production" };

const run = (command, args) =>
  execFileSync(command, args, { cwd: root, stdio: "inherit", env: process.env });

run(bin("cf"), ["workers", "types"]);
run("sh", ["scripts/build-wasm.sh"]);

rmSync(workerDir, { recursive: true, force: true });
run(bin("tsrs"), ["-p", "tsconfig.json", "--fast"]);

for (const name of ["stuff_worker.js", "stuff_worker_bg.wasm"]) {
  const target = join(bundleDir, "build/wasm", name);
  mkdirSync(dirname(target), { recursive: true });
  copyFileSync(join(root, "build/wasm", name), target);
}
for (const stale of ["cloudflare.config.js", "cloudflare.config.js.map"]) {
  rmSync(join(bundleDir, stale), { force: true });
}

const manifest = {
  type: "complete",
  mainModule: "entry.js",
  modules: {
    "entry.js": { type: "esm" },
    "build/wasm/stuff_worker.js": { type: "esm" },
    "build/wasm/stuff_worker_bg.wasm": { type: "wasm" },
  },
};
if (existsSync(join(bundleDir, "entry.js.map"))) {
  manifest.modules["entry.js.map"] = { type: "sourcemap" };
}

const unregister = registerConfigHooks();
const { result } = await loadAndParseConfig(join(root, "cloudflare.config.ts"), buildContext);
unregister();
if (!result.success) {
  for (const issue of result.error.issues) {
    console.error(`${issue.path.join(".")}: ${issue.message}`);
  }
  process.exit(1);
}

const { worker, accountId, complianceRegion } = result.data;
if (worker === undefined) {
  console.error("cloudflare.config.ts has no worker");
  process.exit(1);
}
const { entrypoint: _entrypoint, ...workerConfig } = worker;
writeFileSync(
  join(workerDir, "worker.config.json"),
  JSON.stringify({ ...workerConfig, manifest }),
);
writeFileSync(
  join(outputRoot, "config.json"),
  JSON.stringify({
    ...(accountId === undefined ? {} : { accountId }),
    ...(complianceRegion === undefined ? {} : { complianceRegion }),
    buildContext,
  }),
);

console.log(
  `built ${manifest.mainModule} (${statSync(join(bundleDir, "entry.js")).size} B), ` +
    `wasm (${statSync(join(bundleDir, "build/wasm/stuff_worker_bg.wasm")).size} B)`,
);
