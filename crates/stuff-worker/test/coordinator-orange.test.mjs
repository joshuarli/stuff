import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { spawn, execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { request as httpRequest } from "node:http";
import { join, resolve } from "node:path";
import { generate } from "../scripts/orange.mjs";

const workerDir = resolve(import.meta.dirname, "..");
const root = resolve(workerDir, "../..");
const worker = join(workerDir, ".cloudflare/output/v0/workers/default/bundle/entry.js");
const bin = join(workerDir, "orange/node_modules/open-orange/dist/bin.js");
const digest = (text) => createHash("sha256").update(text).digest("hex");
async function freePort() {
  const server = createServer();
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  const port = server.address().port;
  await new Promise((ok) => server.close(ok));
  return port;
}

test("Orange coordinator serializes activation and repairs projection after restart", { timeout: 120000 }, async (t) => {
  execFileSync("pnpm", ["run", "build"], { cwd: workerDir, stdio: "inherit" });
  const scratch = join(root, ".work/tmp");
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(join(scratch, "coordinator-orange-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const configPath = join(dir, "wrangler.jsonc");
  const config = generate(configPath, "test");
  const wrapper = join(dir, "wrapper.mjs");
  writeFileSync(wrapper, `import worker, { PublicationCoordinator as Base } from ${JSON.stringify(worker)};
let clockOffset = 0;
const actualNow = Date.now;
Date.now = () => actualNow() + clockOffset;
export class PublicationCoordinator extends Base {
 constructor(state, env) {
  const actual = env.MYCRATES;
  const proxy = new Proxy(actual, { get(target, key) { if (key === "put") return async (name, value, options) => { if (name === "current" && await target.get("__fail_projection")) { await target.delete("__fail_projection"); throw new Error("injected projection failure"); } return target.put(name, value, options); }; const v = target[key]; return typeof v === "function" ? v.bind(target) : v; } });
  super(state, { ...env, MYCRATES: proxy });
 }
}
export default { async fetch(req, env, ctx) {
 const u = new URL(req.url);
 if (u.pathname === "/__clock") { clockOffset += 600000; return new Response("ok"); }
 if (u.pathname === "/__seed") { for (const [key, value] of await req.json()) await env.MYCRATES.put(key, value); return new Response("ok"); }
 if (u.pathname === "/__raw") { const x = await env.MYCRATES.get("current"); return new Response(x ? await x.text() : ""); }
 return worker.fetch(req, env, ctx);
} };
`);
  config.main = wrapper;
  config.vars.STUFF_COORDINATOR_ENABLED = "true";
  config.vars.STUFF_COORDINATOR_TOKEN = "test-secret";
  writeFileSync(configPath, JSON.stringify(config));
  const port = await freePort();
  let child;
  async function start() {
    child = spawn(process.execPath, [bin, "dev", "--json", "--ip", "127.0.0.1", "--port", String(port), "--config", configPath], { cwd: dir, stdio: ["ignore", "pipe", "pipe"] });
    let logs = "";
    await new Promise((ok, fail) => {
      child.stdout.on("data", (chunk) => { logs += chunk; for (const line of logs.split("\n")) { try { const event = JSON.parse(line); if (event.type === "ready") ok(); if (event.type === "fatal" || event.type === "error") fail(new Error(line)); } catch (error) { if (!(error instanceof SyntaxError)) fail(error); } } });
      child.stderr.on("data", (chunk) => { logs += chunk; });
      child.once("exit", (code) => fail(new Error(`orange exited ${code}: ${logs}`)));
      setTimeout(() => fail(new Error(`orange readiness timeout: ${logs}`)), 60000).unref();
    });
  }
  async function stop() { if (child && child.exitCode === null) { const done = new Promise((ok) => child.once("exit", ok)); child.kill("SIGTERM"); await done; } }
  t.after(stop);
  const request = (path, method = "GET", body, auth = true, host = "stuff.17166969.xyz") => new Promise((ok, fail) => {
    const req = httpRequest({ host: "127.0.0.1", port, path, method, headers: { host, ...(auth ? { authorization: "Bearer test-secret" } : {}) } }, (res) => { const chunks = []; res.on("data", (chunk) => chunks.push(chunk)); res.on("end", () => ok(new Response(Buffer.concat(chunks), { status: res.statusCode, headers: res.headers }))); });
    req.on("error", fail); req.end(body === undefined ? undefined : JSON.stringify(body));
  });
  const post = (op, body = {}) => request(`/_publication/cargo/${op}`, "POST", body);
  await start();
  const manifest = JSON.stringify({ schemaVersion: 1, objects: [] });
  const content = "generation content";
  const manifest3 = JSON.stringify({ schemaVersion: 1, objects: [{ key: "generations/g3/data", size: content.length, etag: createHash("md5").update(content).digest("hex") }] });
  await request("/__seed", "POST", [["current", "g0"], ...["g0", "g1", "g2"].map((g) => [`manifests/${g}.json`, manifest]), ["manifests/g3.json", manifest3]]);
  assert.equal((await request("/_publication/cargo/current", "GET", undefined, false)).status, 401);
  let lease = await (await post("begin")).json();
  assert.equal((await post("gc-claim")).status, 409);
  const first = { token: lease.token, key: "first", generation: "g1", manifest: digest(manifest), expectedRevision: 0 };
  const second = { ...first, key: "second", generation: "g2" };
  const concurrent = await Promise.all([post("activate", first), post("activate", second)]);
  assert.deepEqual(concurrent.map((r) => r.status).sort(), [200, 409]);
  const winner = concurrent[0].status === 200 ? first : second;
  assert.equal((await post("activate", winner)).status, 200);
  assert.equal((await post("activate", { ...winner, generation: "g3" })).status, 409);
  assert.equal((await post("activate", { ...first, key: "stale" })).status, 409);
  lease = await (await post("begin")).json();
  assert.equal((await post("activate", { ...first, token: lease.token, key: "wrong-revision" })).status, 409);
  assert.equal((await post("gc-claim")).status, 409);
  assert.equal((await post("abort", { token: lease.token })).status, 200);
  const gc = await (await post("gc-claim")).json();
  assert.equal((await post("gc-delete", { token: lease.token, generation: "g0" })).status, 409);
  assert.equal((await post("gc-delete", { token: gc.token, generation: winner.generation })).status, 409);
  assert.equal((await post("gc-delete", { token: gc.token, generation: "g0" })).status, 200);
  assert.equal((await post("abort", { token: gc.token })).status, 200);
  lease = await (await post("begin")).json();
  const next = { token: lease.token, key: "restart", generation: "g3", manifest: digest(manifest3), expectedRevision: 1 };
  assert.equal((await post("activate", next)).status, 409);
  await request("/__seed", "POST", [["generations/g3/data", content], ["__fail_projection", "yes"]]);
  assert.equal((await post("activate", next)).status, 503);
  assert.equal(await (await request("/__raw")).text(), winner.generation);
  await stop();
  await start();
  const restored = await (await request("/_publication/cargo/current")).json();
  assert.equal(restored.revision, 2);
  assert.equal(restored.generation, "g3");
  assert.equal(await (await request("/__raw")).text(), "g3");
  assert.equal((await post("activate", next)).status, 200);
  const served = await request("/current/data", "GET", undefined, false, "crates.17166969.xyz");
  assert.equal(served.status, 200);
  assert.equal(await served.text(), content);
  lease = await (await post("begin")).json();
  const rollback = { token: lease.token, key: "rollback", generation: winner.generation, manifest: digest(manifest), expectedRevision: 2 };
  assert.equal((await post("rollback", rollback)).status, 200);
  assert.equal((await post("rollback", rollback)).status, 200);
  lease = await (await post("begin")).json();
  await request("/__clock", "POST");
  assert.equal((await post("activate", { ...next, token: lease.token, key: "expired", expectedRevision: 3 })).status, 409);
  assert.equal((await post("renew", { token: lease.token })).status, 409);
  const laterGc = await (await post("gc-claim")).json();
  assert.equal((await post("gc-delete", { token: lease.token, generation: "g2" })).status, 409);
  assert.ok(laterGc.token);
  assert.equal((await post("abort", { token: laterGc.token })).status, 200);
  await request("/__seed", "POST", [["manifests/g0.json", manifest]]);
  lease = await (await post("begin")).json();
  assert.equal((await post("activate", { token: lease.token, key: "resurrect-retired", generation: "g0", manifest: digest(manifest), expectedRevision: 3 })).status, 409);
  assert.equal((await post("abort", { token: lease.token })).status, 200);
  await request("/__seed", "POST", [["current", "stale-projection"]]);
  const repaired = await (await request("/_publication/cargo/current")).json();
  assert.equal(repaired.generation, winner.generation);
  assert.equal(await (await request("/__raw")).text(), winner.generation);
  const expiredGc = await (await post("gc-claim")).json();
  await request("/__clock", "POST");
  assert.equal((await post("gc-delete", { token: expiredGc.token, generation: "g2" })).status, 409);
});
