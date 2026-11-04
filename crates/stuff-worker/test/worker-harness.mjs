import { mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createTestHarness } from "wrangler";

const workerDir = fileURLToPath(new URL("../.cloudflare/output/v0/workers/default", import.meta.url));
const bundleDir = join(workerDir, "bundle");
const harnessRoot = fileURLToPath(new URL("../../../.work/tmp", import.meta.url));
const routes = JSON.stringify([
  { host: "crates.test", bucket_binding: "MYCRATES", key_prefix: "" },
  { host: "npm.test", bucket_binding: "MYNPM", key_prefix: "" },
  { host: "pypi.test", bucket_binding: "PYPI", key_prefix: "" },
  { host: "oci.test", bucket_binding: "OCI", key_prefix: "" },
]);

export function workerHarness(vars = {}) {
  process.env.CLOUDFLARE_CF_FETCH_ENABLED = "false";
  mkdirSync(harnessRoot, { recursive: true });
  const config = JSON.parse(readFileSync(join(workerDir, "worker.config.json"), "utf8"));
  return createTestHarness({
    root: harnessRoot,
    workers: [{
      config: {
        name: "stuff-test",
        main: join(bundleDir, "entry.js"),
        compatibility_date: config.compatibilityDate,
        compatibility_flags: config.compatibilityFlags,
        no_bundle: true,
        rules: [
          { type: "ESModule", globs: ["**/*.js"], fallthrough: true },
          { type: "CompiledWasm", globs: ["**/*.wasm"], fallthrough: true },
        ],
        r2_buckets: ["MYCRATES", "MYNPM", "PYPI", "OCI", "OCI_BLOBS"].map((binding) => ({
          binding,
          bucket_name: binding.toLowerCase().replaceAll("_", "-"),
        })),
        vars: {
          STUFF_ROUTES: routes,
          STUFF_UNIFIED_HOST: "stuff.test",
          STUFF_OCI_HOST: "oci.test",
          ...vars,
        },
      },
    }],
  });
}
