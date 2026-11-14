import assert from "node:assert/strict";
import test from "node:test";
import { spawn, execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { request as httpRequest } from "node:http";
import { join, resolve } from "node:path";
import { generate } from "../scripts/orange.mjs";
import { seed, seedOci, OCI_FIXTURE, OCI_ARM64, OCI_LAYER } from "./fixtures.mjs";

const root = resolve(import.meta.dirname, "../../..");
const worker = resolve(import.meta.dirname, "../.cloudflare/output/v0/workers/default/bundle/entry.js");
const bin = resolve(import.meta.dirname, "../orange/node_modules/open-orange/dist/bin.js");

async function port() {
  const server = createServer();
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  const result = server.address().port;
  await new Promise((ok) => server.close(ok));
  return result;
}

test("Orange serves seeded registries and conditional OCI writes", { timeout: 120000 }, async (t) => {
  execFileSync("pnpm", ["run", "build"], { cwd: resolve(import.meta.dirname, ".."), stdio: "inherit" });
  const scratch = join(root, ".work/tmp");
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(join(scratch, "orange-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const configPath = join(dir, "wrangler.jsonc");
  const config = generate(configPath, "test");
  const wrapper = join(dir, "wrapper.mjs");
  writeFileSync(wrapper, `import worker from ${JSON.stringify(worker)};\nexport { PublicationCoordinator } from ${JSON.stringify(worker)};\nexport default { async fetch(req, env, ctx) {\nconst u = new URL(req.url);\nif (u.pathname === "/__seed") { for (const [binding,key,value] of await req.json()) await env[binding].put(key, Uint8Array.from(atob(value), c => c.charCodeAt(0))); return new Response("ok"); }\nif (u.hostname === "other.test") env = { ...env, STUFF_UNIFIED_HOST: "other.test", STUFF_ROUTES: env.STUFF_ROUTES.replace("crates.17166969.xyz", "other-crates.test") };\nreturn worker.fetch(req, env, ctx); } };\n`);
  config.main = wrapper;
  config.vars.STUFF_OCI_PUSH_TOKEN = "token";
  config.vars.STUFF_OCI_PUSH_SECRET = "secret";
  config.vars.STUFF_OCI_WRITER_ORIGIN = "https://oci.17166969.xyz";
  writeFileSync(configPath, JSON.stringify(config));
  const p = await port();
  const child = spawn(process.execPath, [bin, "dev", "--json", "--ip", "127.0.0.1", "--port", String(p), "--config", configPath], { cwd: dir, stdio: ["ignore", "pipe", "pipe"] });
  t.after(async () => { if (child.exitCode === null) { const done = new Promise((ok) => child.once("exit", ok)); child.kill("SIGTERM"); await done; } });
  let logs = "";
  const ready = new Promise((ok, fail) => {
    child.stdout.on("data", (chunk) => { logs += chunk; for (const line of logs.split("\n")) { try { const event = JSON.parse(line); if (event.type === "ready") ok(event.url); if (event.type === "fatal" || event.type === "error") fail(new Error(line)); } catch (error) { if (!(error instanceof SyntaxError)) fail(error); } } });
    child.stderr.on("data", (chunk) => { logs += chunk; });
    child.once("exit", (code) => fail(new Error(`orange exited ${code}: ${logs}`)));
    setTimeout(() => fail(new Error(`orange readiness timeout: ${logs}`)), 60000).unref();
  });
  await ready;
  const puts = [];
  const env = new Proxy({}, { get: (_, binding) => ({ put: async (key, value) => puts.push([binding, key, Buffer.from(value).toString("base64")]) }) });
  const fake = { getWorker: () => ({ getEnv: async () => env }) };
  await seed(fake);
  await seedOci(fake);
  await env.MYCRATES.put("generations/g1/registry/cr/at/crate", "crate-index");
  await env.MYCRATES.put("blobs/crate-1.0.0.crate", "crate-bytes");
  await env.MYNPM.put("generations/g1/package", JSON.stringify({ name: "package", versions: { "1.0.0": { dist: { tarball: "/tarballs/abc.tgz" } } } }));
  await env.MIRROR.put("artifacts/tool.tar.gz", "mirror-bytes");
  const request = (host, path, options = {}) => new Promise((resolveRequest, reject) => {
    const req = httpRequest({ host: "127.0.0.1", port: p, path, method: options.method ?? "GET", headers: { host, ...options.headers } }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => resolveRequest(new Response([204, 205, 304].includes(res.statusCode) || options.method === "HEAD" ? null : Buffer.concat(chunks), { status: res.statusCode, headers: res.headers })));
    });
    req.on("error", reject);
    req.end(options.body);
  });
  const digest = OCI_LAYER.slice(7);
  const layerBytes = readFileSync(join(OCI_FIXTURE, "blobs/sha256", digest));
  const early = [["OCI_BLOBS", `sha256/${digest}`, layerBytes.toString("base64")], ["MYCRATES", "blobs/early.crate", Buffer.from("early-bytes").toString("base64")], ["MYCRATES", "current", Buffer.from("secret").toString("base64")], ["OCI_BLOBS", "secret", Buffer.from("secret").toString("base64")]];
  assert.equal((await request("stuff.17166969.xyz", "/__seed", { method: "POST", body: JSON.stringify(early) })).status, 200);
  assert.equal(await (await request("crates-blobs.17166969.xyz", "/blobs/early.crate")).text(), "early-bytes");
  for (const options of [{}, { method: "HEAD" }, { headers: { range: "bytes=0-3" } }]) {
    const redirect = await request("oci.17166969.xyz", `/v2/example/app/blobs/${OCI_LAYER}`, options);
    assert.equal(redirect.status, 307);
    assert.equal(redirect.headers.get("location"), `https://oci-blobs.17166969.xyz/sha256/${digest}`);
    const target = new URL(redirect.headers.get("location"));
    const response = await request(target.hostname, target.pathname, options);
    assert.equal(response.status, options.headers ? 206 : 200);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), options.method === "HEAD" ? Buffer.alloc(0) : options.headers ? layerBytes.subarray(0, 4) : layerBytes);
  }
  for (const [host, path] of [["crates-blobs.17166969.xyz", "/current"], ["crates-blobs.17166969.xyz", "/blobs/%2e%2e%2fcurrent"], ["crates-blobs.17166969.xyz", "/blobs/%zz"], ["oci-blobs.17166969.xyz", "/secret"], ["oci-blobs.17166969.xyz", "/sha256/%2e%2e%2fsecret"], ["oci-blobs.17166969.xyz", "/sha256/abc"]]) {
    assert.equal((await request(host, path)).status, 404, `${host}${path}`);
  }
  assert.equal((await request("stuff.17166969.xyz", "/__seed", { method: "POST", body: JSON.stringify(puts) })).status, 200);
  for (const path of ["/", "/llms.txt"]) {
    const page = await request("stuff.17166969.xyz", path); const body = await page.text(); assert.equal(page.status, 200, body); assert.ok(body.includes("crate")); assert.ok(body.includes("//crates.17166969.xyz")); assert.ok(!body.includes("//crates-blobs.17166969.xyz"));
    const other = await request("other.test", path); assert.equal(other.status, 200); assert.notEqual(await other.text(), body);
  }
  for (const [host, path, expected] of [
    ["crates.17166969.xyz", "/generations/g1/registry/cr/at/crate", "crate-index"],
    ["npm.17166969.xyz", "/generations/g1/-/tarballs/abc.tgz", "tarball-bytes"],
    ["pypi.17166969.xyz", "/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl", "wheel-bytes"],
  ]) {
    const response = await request(host, path);
    const body = await response.text();
    assert.equal(response.status, 200, `${host}${path}: ${body}`);
    assert.ok(body.includes(expected));
  }
  const wheelPath = "/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl";
  const wheel = await request("pypi.17166969.xyz", wheelPath); assert.equal(wheel.status, 200); assert.equal(await wheel.text(), "wheel-bytes");
  assert.equal((await request("pypi.17166969.xyz", "/generations/g1/simple/wheelpkg/index.html")).status, 200);
  const head = await request("pypi.17166969.xyz", wheelPath, { method: "HEAD" }); assert.equal(head.status, 200); assert.equal(await head.text(), "");
  assert.equal((await request("pypi.17166969.xyz", wheelPath, { headers: { "if-none-match": head.headers.get("etag") } })).status, 304);
  const range = await request("pypi.17166969.xyz", wheelPath, { headers: { range: "bytes=0-3" } }); assert.equal(range.status, 206); assert.equal(await range.text(), "whee");
  assert.equal((await request("pypi.17166969.xyz", wheelPath, { headers: { range: "bytes=100-200" } })).status, 416);
  assert.equal((await request("oci.17166969.xyz", "/v2/example/app/manifests/stable")).status, 200);
  assert.equal((await request("oci.17166969.xyz", `/v2/example/app/blobs/${OCI_LAYER}`)).status, 307);
  assert.equal((await request("oci.17166969.xyz", `/v2/example/app/referrers/${OCI_ARM64}`)).status, 200);
  assert.equal((await request("oci.17166969.xyz", "/v2/example/app/tags/list")).status, 200);
  const manifest = JSON.parse(readFileSync(join(OCI_FIXTURE, "blobs/sha256", OCI_ARM64.slice(7)), "utf8")); manifest.annotations = { title: "orange" };
  const pushed = await request("oci.17166969.xyz", "/v2/example/app/manifests/orange", { method: "PUT", headers: { authorization: `Basic ${Buffer.from("stuff:token").toString("base64")}`, "content-type": "application/vnd.oci.image.manifest.v1+json" }, body: JSON.stringify(manifest) }); assert.equal(pushed.status, 201);
});
