import { cpSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { generate } from "./orange.mjs";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const bundle = join(root, ".cloudflare/output/v0/workers/default/bundle");

export function stage(destination, release) {
  const app = resolve(destination);
  mkdirSync(app, { recursive: true });
  const descriptor = join(app, "stuff.jsonc");
  const config = generate(descriptor, release);
  const main = relative(bundle, config.main);
  if (!main || main.startsWith("..") || resolve(bundle, main) !== config.main) throw new Error("worker main is outside bundle");
  cpSync(bundle, join(app, "bundle"), { recursive: true });
  config.main = `./bundle/${main}`;
  writeFileSync(descriptor, `${JSON.stringify(config, null, 2)}\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [destination, release] = process.argv.slice(2);
  if (!destination || !release) throw new Error("usage: node scripts/stage-orange.mjs APP RELEASE");
  stage(destination, release);
}
