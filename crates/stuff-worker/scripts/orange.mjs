import { readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const output = join(root, ".cloudflare/output/v0/workers/default");
const keys = (value, allowed, context) => {
  for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new Error(`unmapped ${context} field: ${key}`);
};
export function generate(destination, release) {
  if (!release) throw new Error("expected a release label");
  const worker = JSON.parse(readFileSync(join(output, "worker.config.json"), "utf8"));
  keys(worker, ["name", "compatibilityDate", "compatibilityFlags", "domains", "cache", "observability", "env", "exports", "manifest"], "worker");
  keys(worker.manifest, ["type", "mainModule", "modules"], "manifest");
  const vars = {};
  const r2_buckets = [];
  const durable = [];
  const classes = Object.entries(worker.exports ?? {}).map(([name, value]) => {
    keys(value, ["type", "storage"], `export ${name}`);
    if (value.type !== "durable-object" || value.storage !== "sqlite") throw new Error(`unmapped export ${name}`);
    return name;
  });
  for (const [name, binding] of Object.entries(worker.env)) {
    if (binding.type === "text") { keys(binding, ["type", "value"], name); vars[name] = binding.value; }
    else if (binding.type === "r2") { keys(binding, ["type", "name"], name); r2_buckets.push({ binding: name, bucket_name: binding.name }); }
    else if (binding.type === "durable-object") { keys(binding, ["type", "worker", "exportName"], name); if (binding.worker !== worker.name) throw new Error("external durable object"); durable.push({ name, class_name: binding.exportName }); }
    else throw new Error(`unmapped binding ${name}: ${binding.type}`);
  }
  vars.STUFF_S3_BUCKET_BINDINGS = JSON.stringify(Object.fromEntries(r2_buckets.map(({ binding, bucket_name }) => [binding, bucket_name])));
  vars.STUFF_RELEASE = release;
  if (durable.some(binding => !classes.includes(binding.class_name)) || classes.some(name => !durable.some(binding => binding.class_name === name))) throw new Error("unmapped durable object class");
  const config = { name: worker.name, main: join(output, "bundle", worker.manifest.mainModule), no_bundle: true, compatibility_date: worker.compatibilityDate, compatibility_flags: worker.compatibilityFlags ?? [], vars, r2_buckets, durable_objects: { bindings: durable }, migrations: [{ tag: "v1", new_sqlite_classes: classes }] };
  writeFileSync(destination, `${JSON.stringify(config, null, 2)}\n`);
  return config;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [destination, release] = process.argv.slice(2);
  if (!destination) throw new Error("usage: node scripts/orange.mjs OUTPUT RELEASE");
  generate(resolve(destination), release);
}
