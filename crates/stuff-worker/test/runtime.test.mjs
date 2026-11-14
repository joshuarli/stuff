import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync, spawn } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { fileURLToPath } from "node:url";
import { join, posix } from "node:path";
import { workerHarness } from "./worker-harness.mjs";
import { catalog, seed, seedOci, OCI_FIXTURE, OCI_INDEX, OCI_ARM64, OCI_AMD64, OCI_REFERRER, OCI_LAYER, OCI_REFERRER_TYPE } from "./fixtures.mjs";

const outputDir = fileURLToPath(
  new URL("../.cloudflare/output/v0/workers/default", import.meta.url),
);
const bundleDir = join(outputDir, "bundle");

test("the uploaded module graph resolves from the main module name", () => {
  const { manifest } = JSON.parse(readFileSync(join(outputDir, "worker.config.json"), "utf8"));
  const graph = new Map();
  for (const [name, module] of Object.entries(manifest.modules)) {
    if (module.type === "esm") {
      graph.set(name, readFileSync(join(bundleDir, name), "utf8"));
    }
  }
  const mainName = posix.basename(manifest.mainModule);
  graph.set(mainName, readFileSync(join(bundleDir, manifest.mainModule), "utf8"));
  const names = new Set([...Object.keys(manifest.modules), mainName]);
  for (const [name, source] of graph) {
    for (const [, specifier] of source.matchAll(/(?:from\s+|import\s*)["']([^"']+)["']/g)) {
      if (!specifier.startsWith(".")) continue;
      const resolved = posix.normalize(posix.join(posix.dirname(name), specifier));
      assert.ok(names.has(resolved), `${name} imports ${specifier}, uploads as ${resolved}`);
    }
  }
});

test("cold HEAD for the unified page uses a GET cache key", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const head = await server.getWorker().fetch("https://stuff.test/", { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
  const get = await server.getWorker().fetch("https://stuff.test/");
  assert.equal(get.status, 200);
  assert.match(await get.text(), /stuff/);
});

test("a registry on a newer catalog schema is reported, not fatal", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  const newer = JSON.stringify({ schema_version: 99, registry: "pypi", generation: "g1", packages: [] });
  await seed(server, { pypiCatalog: newer });

  const page = await server.getWorker().fetch("https://stuff.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.match(html, /PyPI is not listed because its catalog could not be read: unsupported catalog schema_version: 99\./);
  assert.ok(html.includes("crate") && html.includes("package"));

  const llms = await server.getWorker().fetch("https://stuff.test/llms.txt");
  assert.equal(llms.status, 200);
  assert.match(await llms.text(), /> Note: PyPI is not listed because its catalog could not be read/);
});

async function promote(server, binding, generation, files) {
  const bucket = (await server.getWorker().getEnv())[binding];
  for (const [path, text] of Object.entries(files)) {
    await bucket.put(`generations/${generation}/${path}`, text);
  }
  await bucket.put("current", generation);
}

test("the unified page follows a moved pointer", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const before = await server.getWorker().fetch("https://stuff.test/");
  assert.equal(before.status, 200);
  assert.ok(!(await before.text()).includes("promoted-crate"));
  await promote(server, "MYCRATES", "g2", { "catalog.json": catalog("cargo", "promoted-crate") });
  const after = await server.getWorker().fetch("https://stuff.test/");
  assert.equal(after.status, 200);
  assert.ok((await after.text()).includes("promoted-crate"));
  const head = await server.getWorker().fetch("https://stuff.test/", { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
});

test("pointer routes follow a moved pointer on consecutive reads", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  for (let index = 0; index < 3; index += 1) {
    const first = await server.getWorker().fetch("https://crates.test/llms.txt");
    assert.equal(await first.text(), "# generation");
  }
  await promote(server, "MYCRATES", "g2", { "llms.txt": "# promoted" });
  const moved = await server.getWorker().fetch("https://crates.test/llms.txt");
  assert.equal(moved.status, 200);
  assert.equal(await moved.text(), "# promoted");
  const etag = moved.headers.get("etag");
  const revalidated = await server.getWorker().fetch("https://crates.test/llms.txt", {
    headers: { "if-none-match": etag },
  });
  assert.equal(revalidated.status, 304);
  await promote(server, "MYCRATES", "g3", { "llms.txt": "# third" });
  const stale = await server.getWorker().fetch("https://crates.test/llms.txt", {
    headers: { "if-none-match": etag },
  });
  assert.equal(stale.status, 200);
  assert.equal(await stale.text(), "# third");
});

test("unified llms.txt advertises the configured hosts", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const response = await server.getWorker().fetch("https://stuff.test/llms.txt");
  assert.equal(response.status, 200);
  const text = await response.text();
  assert.match(text, /Aggregate browser: https:\/\/stuff\.test\/ /);
  for (const origin of ["https://crates.test", "https://npm.test", "https://pypi.test", "https://oci.test", "https://mirror.test"]) {
    assert.ok(text.includes(`${origin}/llms.txt`), `${origin}/llms.txt`);
  }
  assert.ok(!text.includes("17166969.xyz"));
});

test("the unified page and llms.txt include the mirror", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const page = await server.getWorker().fetch("https://stuff.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.ok(html.includes("tool-1.0"), "unified page lists the mirror artifact");
  assert.ok(html.includes('id="r-mirror"'), "unified page has the mirror section");
  const llms = await server.getWorker().fetch("https://stuff.test/llms.txt");
  const text = await llms.text();
  assert.ok(text.includes("Static artifact mirror"), "unified llms names the mirror");
  assert.ok(text.includes("- Mirror: https://mirror.test/llms.txt"), "unified llms links the mirror");
});

test("the unified page and llms.txt include OCI", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const page = await server.getWorker().fetch("https://stuff.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.ok(html.includes("example/app"), "unified page lists the OCI repository");
  assert.ok(html.includes("OCI"), "unified page names the OCI registry");
  const llms = await server.getWorker().fetch("https://stuff.test/llms.txt");
  const text = await llms.text();
  assert.ok(text.includes("OCI registry"), "unified llms names the OCI registry");
  assert.ok(text.includes("- OCI: https://oci.test/llms.txt"), "unified llms links OCI");
  const catalog = await server.getWorker().fetch("https://oci.test/current/catalog.json");
  assert.equal(catalog.status, 200);
  assert.match(await catalog.text(), /"registry":"oci"/);
});

test("root pages link to objects that resolve", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const page = await server.getWorker().fetch("https://crates.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  const links = [...html.matchAll(/href="([^"]+)"/g)].map((match) => match[1]);
  assert.ok(links.length > 0);
  for (const href of links) {
    if (href.startsWith("http")) continue;
    const response = await server.getWorker().fetch(new URL(href, "https://crates.test/"));
    assert.equal(response.status, 200, `${href} should resolve`);
  }
  const alias = await server.getWorker().fetch("https://crates.test/catalog.json");
  assert.equal(alias.status, 200);
});

test("the gateway forwards the host to the cached entrypoint", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const cargo = await server.getWorker().fetch("https://crates.test/catalog.json");
  const npm = await server.getWorker().fetch("https://npm.test/catalog.json");
  assert.equal(cargo.status, 200);
  assert.equal(npm.status, 200);
  assert.match(await cargo.text(), /"registry":"cargo"/);
  assert.match(await npm.text(), /"registry":"npm"/);
});

test("pinned generation artifacts use the shared store aliases", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const wheel = await server.getWorker().fetch(
    "https://pypi.test/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl",
  );
  assert.equal(wheel.status, 200);
  assert.equal(await wheel.text(), "wheel-bytes");
  const tarball = await server.getWorker().fetch(
    "https://npm.test/generations/g1/-/tarballs/abc.tgz",
  );
  assert.equal(tarball.status, 200);
  assert.equal(await tarball.text(), "tarball-bytes");
  const missingGeneration = await server.getWorker().fetch(
    "https://pypi.test/generations/g2/wheels/wheelpkg-1.0-py3-none-any.whl",
  );
  assert.equal(missingGeneration.status, 404);
});

test("a ranged artifact request returns only the requested bytes", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const response = await server.getWorker().fetch(
    "https://pypi.test/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl",
    { headers: { range: "bytes=0-4" } },
  );
  assert.equal(response.status, 206);
  assert.equal(response.headers.get("content-range"), "bytes 0-4/11");
  assert.equal(await response.text(), "wheel");
  const missing = await server.getWorker().fetch("https://pypi.test/nope");
  assert.equal(missing.status, 404);
  assert.match(missing.headers.get("cache-control") ?? "", /max-age=60/);
});

test("an ignored multi-range request returns the full body", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const response = await server.getWorker().fetch(
    "https://pypi.test/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl",
    { headers: { range: "bytes=0-0,2-2" } },
  );
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-length"), "11");
  assert.equal(await response.text(), "wheel-bytes");
});

test("a pinned project page's relative wheel link resolves", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seed(server);
  const pageUrl = "https://pypi.test/generations/g1/simple/wheelpkg/index.html";
  const page = await server.getWorker().fetch(pageUrl);
  assert.equal(page.status, 200);
  const html = await page.text();
  const href = html.match(/href="([^"]+)"/)[1];
  const wheel = await server.getWorker().fetch(new URL(href, pageUrl));
  assert.equal(wheel.status, 200);
  assert.equal(await wheel.text(), "wheel-bytes");
});

const REPO_ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const DIGEST = "ab".repeat(64);
const TARBALL = "tarball-bytes";
const WHEEL = "wheel-bytes";

function paritySite() {
  const work = join(REPO_ROOT, ".work", "tmp");
  mkdirSync(work, { recursive: true });
  const site = mkdtempSync(join(work, "parity-site-"));
  const generation = join(site, "generations", "g1");
  mkdirSync(join(generation, "registry"), { recursive: true });
  mkdirSync(join(generation, "simple", "proj"), { recursive: true });
  mkdirSync(join(generation, "-", "tarballs"), { recursive: true });
  mkdirSync(join(site, "tarballs"), { recursive: true });
  mkdirSync(join(site, "wheels"), { recursive: true });
  writeFileSync(join(generation, "catalog.json"), '{"schema_version":1,"registry":"cargo","generation":"g1","packages":[]}\n');
  writeFileSync(join(generation, "index.html"), "<html>root</html>");
  writeFileSync(join(generation, "llms.txt"), "# Cargo registry\n");
  writeFileSync(join(generation, "registry", "config.json"), "{}");
  writeFileSync(join(generation, "simple", "index.html"), "<html>simple</html>");
  writeFileSync(join(generation, "simple", "proj", "index.html"), "<html>project</html>");
  writeFileSync(
    join(generation, "flat"),
    JSON.stringify({
      name: "flat",
      versions: {
        "1.2.3": { dist: { tarball: `/current/-/tarballs/${DIGEST}.tgz` } },
      },
    }),
  );
  writeFileSync(join(site, "tarballs", `${DIGEST}.tgz`), TARBALL);
  copyFileSync(join(site, "tarballs", `${DIGEST}.tgz`), join(generation, "-", "tarballs", `${DIGEST}.tgz`));
  writeFileSync(join(site, "wheels", "pkg-1.0-py3-none-any.whl"), WHEEL);
  symlinkSync("../../wheels", join(generation, "wheels"));
  symlinkSync("generations/g1", join(site, "current"));
  return site;
}

async function seedParity(server) {
  const bucket = (await server.getWorker().getEnv())["MYCRATES"];
  await bucket.put("current", "g1");
  await bucket.put(
    "generations/g1/catalog.json",
    '{"schema_version":1,"registry":"cargo","generation":"g1","packages":[]}',
  );
  await bucket.put("generations/g1/index.html", "<html>root</html>");
  await bucket.put("generations/g1/llms.txt", "# Cargo registry\n");
  await bucket.put("generations/g1/registry/config.json", "{}");
  await bucket.put("generations/g1/simple/index.html", "<html>simple</html>");
  await bucket.put("generations/g1/simple/proj/index.html", "<html>project</html>");
  await bucket.put(
    "generations/g1/flat",
    JSON.stringify({
      name: "flat",
      versions: {
        "1.2.3": { dist: { tarball: `/current/-/tarballs/${DIGEST}.tgz` } },
      },
    }),
  );
  await bucket.put(`tarballs/${DIGEST}.tgz`, TARBALL);
  await bucket.put("wheels/pkg-1.0-py3-none-any.whl", WHEEL);
}

function startServer(site) {
  const bin = process.env.STUFF_SERVER_BIN ?? join(REPO_ROOT, "target", "debug", "stuff-server");
  execFileSync("cargo", ["build", "-q", "-p", "stuff-server"], { cwd: REPO_ROOT });
  const child = spawn(bin, ["--bind", "127.0.0.1:0", "--route", `/=${site}`], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  return new Promise((resolve, reject) => {
    let output = "";
    child.stdout.on("data", (chunk) => {
      output += chunk;
      const match = output.match(/listening on ([^\n]+)/);
      if (match) {
        resolve({ child, base: `http://${match[1].trim()}` });
      }
    });
    child.on("error", reject);
    child.on("exit", (code) => reject(new Error(`stuff-server exited with ${code}`)));
  });
}

test("native server and worker agree on routes and cache kinds", async (t) => {
  const site = paritySite();
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seedParity(server);
  const nativeServer = await startServer(site);
  t.after(() => {
    nativeServer.child.kill();
  });

  const paths = [
    "/",
    "/index.html",
    "/catalog.json",
    "/llms.txt",
    "/simple",
    "/simple/",
    "/simple/proj",
    "/simple/proj/",
    "/current/simple/proj",
    "/current/simple/proj/",
    `/current/-/tarballs/${DIGEST}.tgz`,
    `/generations/g1/-/tarballs/${DIGEST}.tgz`,
    "/current/flat/-/flat-1.2.3.tgz",
    "/current/wheels/pkg-1.0-py3-none-any.whl",
    "/generations/g1/wheels/pkg-1.0-py3-none-any.whl",
    "/generations/missing/wheels/pkg-1.0-py3-none-any.whl",
    `/tarballs/${DIGEST}.tgz`,
    "/current/registry/config.json",
    "/generations/g1/registry/config.json",
    "/missing",
  ];
  for (const path of paths) {
    const native = await fetch(`${nativeServer.base}${path}`);
    const edge = await server.getWorker().fetch(`https://crates.test${path}`);
    assert.equal(native.status, edge.status, `status for ${path}`);
    assert.equal(
      (native.headers.get("cache-control") ?? "").replace(/\s+/g, " "),
      (edge.headers.get("cache-control") ?? "").replace(/\s+/g, " "),
      `cache-control for ${path}`,
    );
  }
});

test("oci ping and manifest reads ignore Accept and pin the stored type", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);

  const ping = await server.getWorker().fetch("https://oci.test/v2/");
  assert.equal(ping.status, 200);
  assert.equal(ping.headers.get("docker-distribution-api-version"), "registry/2.0");
  assert.equal(await ping.text(), "{}");

  const byTag = await server.getWorker().fetch("https://oci.test/v2/example/app/manifests/stable", {
    headers: { accept: "application/vnd.docker.distribution.manifest.list.v2+json" },
  });
  assert.equal(byTag.status, 200);
  assert.equal(byTag.headers.get("content-type"), "application/vnd.oci.image.index.v1+json");
  assert.equal(byTag.headers.get("docker-content-digest"), OCI_INDEX);
  assert.equal(byTag.headers.get("cache-control"), "public, max-age=60, stale-if-error=0");
  assert.equal((await byTag.arrayBuffer()).byteLength, 491);

  const byDigest = await server.getWorker().fetch(`https://oci.test/v2/example/app/manifests/${OCI_INDEX}`);
  assert.equal(byDigest.status, 200);
  assert.match(byDigest.headers.get("cache-control"), /immutable/);
  const head = await server.getWorker().fetch(`https://oci.test/v2/example/app/manifests/${OCI_INDEX}`, {
    method: "HEAD",
  });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
  assert.equal(head.headers.get("content-length"), "491");
});

test("oci blob reads support HEAD and a single range", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const base = `https://oci.test/v2/example/app/blobs/${OCI_LAYER}`;

  const full = await server.getWorker().fetch(base);
  assert.equal(full.status, 200);
  assert.equal(full.headers.get("docker-content-digest"), OCI_LAYER);
  assert.equal((await full.arrayBuffer()).byteLength, 117);

  const head = await server.getWorker().fetch(base, { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(head.headers.get("content-length"), "117");
  assert.equal(await head.text(), "");

  const ranged = await server.getWorker().fetch(base, { headers: { range: "bytes=0-9" } });
  assert.equal(ranged.status, 206);
  assert.equal(ranged.headers.get("content-range"), "bytes 0-9/117");
  const bytes = Buffer.from(await ranged.arrayBuffer());
  assert.deepEqual(
    bytes,
    readFileSync(join(OCI_FIXTURE, "blobs", "sha256", OCI_LAYER.slice("sha256:".length))).subarray(0, 10),
  );

  const missing = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/blobs/sha256:1111111111111111111111111111111111111111111111111111111111111111",
  );
  assert.equal(missing.status, 404);
  assert.match(await missing.text(), /BLOB_UNKNOWN/);
});

test("oci tags paginate and referrers honor artifactType", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);

  const page1 = await server.getWorker().fetch("https://oci.test/v2/example/app/tags/list?n=1");
  assert.equal(page1.status, 200);
  assert.deepEqual(await page1.json(), { name: "example/app", tags: ["beta"] });
  assert.equal(
    page1.headers.get("link"),
    '</v2/example/app/tags/list?n=1&last=beta>; rel="next"',
  );
  const page2 = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/tags/list?n=1&last=beta",
  );
  assert.deepEqual(await page2.json(), { name: "example/app", tags: ["stable"] });
  assert.equal(page2.headers.get("link"), null);

  const all = await server.getWorker().fetch(`https://oci.test/v2/example/app/referrers/${OCI_ARM64}`);
  assert.equal(all.status, 200);
  const allBody = await all.json();
  assert.equal(allBody.manifests.length, 1);
  assert.equal(allBody.manifests[0].digest, OCI_REFERRER);

  const filtered = await server.getWorker().fetch(
    `https://oci.test/v2/example/app/referrers/${OCI_ARM64}?artifactType=${encodeURIComponent(OCI_REFERRER_TYPE)}`,
  );
  assert.equal(filtered.headers.get("oci-filters-applied"), "artifactType");
  assert.equal((await filtered.json()).manifests.length, 1);

  const unknown = await server.getWorker().fetch(
    `https://oci.test/v2/example/app/referrers/${OCI_ARM64}?artifactType=${encodeURIComponent("application/vnd.example.unknown")}`,
  );
  assert.deepEqual((await unknown.json()).manifests, []);
});

test("oci errors are OCI JSON with the expected status", async (t) => {
  const server = workerHarness();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const unknownManifest = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/manifests/nope",
  );
  assert.equal(unknownManifest.status, 404);
  assert.match(await unknownManifest.text(), /MANIFEST_UNKNOWN/);

  const unknownRepository = await server.getWorker().fetch("https://oci.test/v2/other/repo/tags/list");
  assert.equal(unknownRepository.status, 404);
  assert.match(await unknownRepository.text(), /NAME_UNKNOWN/);

  const badName = await server.getWorker().fetch("https://oci.test/v2/Example/app/manifests/stable");
  assert.equal(badName.status, 400);
  assert.match(await badName.text(), /NAME_INVALID/);

  const badDigest = await server.getWorker().fetch("https://oci.test/v2/example/app/blobs/sha256:00");
  assert.equal(badDigest.status, 400);
  assert.match(await badDigest.text(), /DIGEST_INVALID/);

  const write = await server.getWorker().fetch("https://oci.test/v2/example/app/blobs/uploads/", {
    method: "POST",
  });
  assert.equal(write.status, 405);
  assert.equal(write.headers.get("allow"), "GET, HEAD");
  assert.match(await write.text(), /UNSUPPORTED/);
});

test("oci blobs redirect to the configured blob origin", async (t) => {
  const server = workerHarness({ STUFF_OCI_BLOB_ORIGIN: "https://blobs.test/" });
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const url = `https://oci.test/v2/example/app/blobs/${OCI_LAYER}`;
  for (const method of ["GET", "HEAD"]) {
    const response = await server.getWorker().fetch(url, { method, redirect: "manual" });
    assert.equal(response.status, 307, method);
    assert.equal(
      response.headers.get("location"),
      `https://blobs.test/sha256/${OCI_LAYER.slice("sha256:".length)}`,
      method,
    );
    assert.equal(response.headers.get("docker-content-digest"), OCI_LAYER, method);
  }
});

function pushWorker() {
  return workerHarness({
    STUFF_OCI_PUSH_TOKEN: "token",
    STUFF_OCI_PUSH_SECRET: "secret",
    STUFF_OCI_WRITER_ORIGIN: "https://writer.test",
  });
}

const AUTH = {
  authorization: `Basic ${Buffer.from("stuff:token").toString("base64")}`,
};

function pushedManifest() {
  const blobDir = join(OCI_FIXTURE, "blobs", "sha256");
  const manifest = JSON.parse(readFileSync(join(blobDir, OCI_ARM64.slice("sha256:".length)), "utf8"));
  manifest.annotations = { "org.opencontainers.image.title": "pushed" };
  return JSON.stringify(manifest);
}

function pushedIndex() {
  const blobDir = join(OCI_FIXTURE, "blobs", "sha256");
  const index = JSON.parse(readFileSync(join(blobDir, OCI_INDEX.slice("sha256:".length)), "utf8"));
  index.annotations = { "org.opencontainers.image.title": "pushed-index" };
  return JSON.stringify(index);
}

test("oci push authenticates, issues writer sessions, and mounts", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);

  const unauthenticated = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/blobs/uploads/",
    { method: "POST" },
  );
  assert.equal(unauthenticated.status, 401);
  assert.equal(
    unauthenticated.headers.get("www-authenticate"),
    'Basic realm="stuff-oci"',
  );

  const started = await server.getWorker().fetch("https://oci.test/v2/example/app/blobs/uploads/", {
    method: "POST",
    headers: AUTH,
  });
  assert.equal(started.status, 202);
  const location = started.headers.get("location") ?? "";
  assert.match(location, /^https:\/\/writer\.test\/v2\/example\/app\/blobs\/uploads\/[0-9a-f]+\?session=/);
  assert.ok(started.headers.get("docker-upload-uuid"));

  const mounted = await server.getWorker().fetch(
    `https://oci.test/v2/example/app/blobs/uploads/?mount=${OCI_LAYER}&from=example/app`,
    { method: "POST", headers: AUTH },
  );
  assert.equal(mounted.status, 201);
  assert.equal(mounted.headers.get("docker-content-digest"), OCI_LAYER);

  const missed = await server.getWorker().fetch(
    `https://oci.test/v2/example/app/blobs/uploads/?mount=sha256:${"1".repeat(64)}&from=example/app`,
    { method: "POST", headers: AUTH },
  );
  assert.equal(missed.status, 202);
});

test("oci push finalizes manifests and flips current", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const meta = (await server.getWorker().getEnv())["OCI"];
  const before = (await (await meta.get("current")).text()).trim();

  const body = pushedManifest();
  const pushed = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/manifests/pushed",
    {
      method: "PUT",
      headers: {
        ...AUTH,
        "content-type": "application/vnd.oci.image.manifest.v1+json",
      },
      body,
    },
  );
  assert.equal(pushed.status, 201);
  const digest = pushed.headers.get("docker-content-digest");
  assert.match(digest ?? "", /^sha256:[0-9a-f]{64}$/);
  const current = (await (await meta.get("current")).text()).trim();
  assert.notEqual(current, before);
  assert.equal(current.length, 24);
  assert.ok(await meta.head(`generations/${current}/repositories/example/app/repository.json`));
  assert.ok(await meta.head(`manifests/${current}.json`));

  const read = await server.getWorker().fetch("https://oci.test/v2/example/app/manifests/pushed");
  assert.equal(read.status, 200);
  assert.equal(await read.text(), body);

  const index = pushedIndex();
  const indexed = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/manifests/pushed-index",
    {
      method: "PUT",
      headers: {
        ...AUTH,
        "content-type": "application/vnd.oci.image.index.v1+json",
      },
      body: index,
    },
  );
  assert.equal(indexed.status, 201);
  const indexRead = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/manifests/pushed-index",
  );
  assert.equal(indexRead.status, 200);
  assert.equal(await indexRead.text(), index);
});

test("oci push rejects a missing blob and leaves current unchanged", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const meta = (await server.getWorker().getEnv())["OCI"];
  const before = (await (await meta.get("current")).text()).trim();
  const missing = JSON.stringify({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: {
      mediaType: "application/vnd.oci.image.config.v1+json",
      digest: `sha256:${"1".repeat(64)}`,
      size: 2,
    },
    layers: [],
  });
  const response = await server.getWorker().fetch(
    "https://oci.test/v2/example/app/manifests/missing",
    {
      method: "PUT",
      headers: {
        ...AUTH,
        "content-type": "application/vnd.oci.image.manifest.v1+json",
      },
      body: missing,
    },
  );
  assert.equal(response.status, 400);
  assert.match(await response.text(), /MANIFEST_BLOB_UNKNOWN/);
  const after = (await (await meta.get("current")).text()).trim();
  assert.equal(after, before);
});

function putManifest(server, repository, tag, body) {
  return server.getWorker().fetch(`https://oci.test/v2/${repository}/manifests/${tag}`, {
    method: "PUT",
    headers: {
      ...AUTH,
      "content-type": "application/vnd.oci.image.manifest.v1+json",
    },
    body,
  });
}

async function assertTagsServe(server, repository, bodies) {
  for (const [tag, body] of Object.entries(bodies)) {
    const read = await server.getWorker().fetch(`https://oci.test/v2/${repository}/manifests/${tag}`);
    assert.equal(read.status, 200, `${repository}:${tag}`);
    assert.equal(await read.text(), body, `${repository}:${tag}`);
  }
}

test("oci push keeps every acknowledged tag under a concurrent current flip", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const bodies = {
    "concurrent-a": pushedManifest().replace("pushed", "concurrent-a"),
    "concurrent-b": pushedManifest().replace("pushed", "concurrent-b"),
    "concurrent-c": pushedManifest().replace("pushed", "concurrent-c"),
  };
  const responses = await Promise.all(
    Object.entries(bodies).map(([tag, body]) => putManifest(server, "example/app", tag, body)),
  );
  assert.deepEqual(responses.map((response) => response.status), [201, 201, 201]);
  await assertTagsServe(server, "example/app", bodies);
});

test("oci first publications race without losing an acknowledged tag", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const meta = (await server.getWorker().getEnv())["OCI"];
  await meta.delete("current");
  const tags = ["first-a", "first-b", "first-c", "first-d"];
  const bodies = Object.fromEntries(tags.map((tag) => [tag, pushedManifest().replace("pushed", tag)]));
  const responses = await Promise.all(
    tags.map((tag) => putManifest(server, "example/app", tag, bodies[tag])),
  );
  assert.deepEqual(responses.map((response) => response.status), tags.map(() => 201));
  await assertTagsServe(server, "example/app", bodies);
});

test("oci push preserves other repositories and referrers across listing pages", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const meta = (await server.getWorker().getEnv())["OCI"];
  const referrers = JSON.stringify({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.index.v1+json",
    manifests: [],
  });
  const fillers = Array.from({ length: 1000 }, (_, index) => index.toString(16).padStart(64, "f"));
  for (let start = 0; start < fillers.length; start += 100) {
    await Promise.all(
      fillers.slice(start, start + 100).map((hex) =>
        meta.put(`generations/g1/repositories/example/app/referrers/sha256/${hex}.json`, referrers),
      ),
    );
  }
  const body = pushedManifest().replace("pushed", "other");
  const pushed = await putManifest(server, "other/repo", "latest", body);
  assert.equal(pushed.status, 201);
  await assertTagsServe(server, "other/repo", { latest: body });

  const stable = await server.getWorker().fetch("https://oci.test/v2/example/app/manifests/stable");
  assert.equal(stable.status, 200, "a repository past the first listing page survives");
  const discovered = await server.getWorker().fetch(
    `https://oci.test/v2/example/app/referrers/${OCI_ARM64}`,
  );
  assert.equal(discovered.status, 200);
  assert.deepEqual(
    (await discovered.json()).manifests.map((manifest) => manifest.digest),
    [OCI_REFERRER],
  );
});

test("oci push refuses to rebuild from an incomplete prior generation", async (t) => {
  const server = pushWorker();
  t.after(() => server.close());
  await server.listen();
  await seedOci(server);
  const meta = (await server.getWorker().getEnv())["OCI"];
  await meta.delete("generations/g1/blobs.json");
  const pushed = await putManifest(server, "example/app", "pushed", pushedManifest());
  assert.equal(pushed.status, 500);
  assert.equal((await (await meta.get("current")).text()).trim(), "g1");
});
