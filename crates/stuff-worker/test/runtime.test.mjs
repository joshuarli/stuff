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
import { Miniflare } from "miniflare";

const outputDir = fileURLToPath(
  new URL("../.cloudflare/output/v0/workers/default", import.meta.url),
);
const bundleDir = join(outputDir, "bundle");

const ROUTES = JSON.stringify([
  { host: "crates.test", bucket_binding: "MYCRATES", key_prefix: "" },
  { host: "npm.test", bucket_binding: "MYNPM", key_prefix: "" },
  { host: "pypi.test", bucket_binding: "PYPI", key_prefix: "" },
  { host: "oci.test", bucket_binding: "OCI", key_prefix: "" },
]);

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

function catalog(registry, name) {
  return JSON.stringify({
    schema_version: 1,
    registry,
    generation: "g1",
    packages: [
      {
        name,
        source: registry === "cargo" ? "native" : null,
        latest: "1.0.0",
        versions: [{ version: "1.0.0", yanked: false }],
      },
    ],
  });
}

async function seed(mf, { pypiCatalog } = {}) {
  const catalogs = {
    MYCRATES: catalog("cargo", "crate"),
    MYNPM: catalog("npm", "package"),
    PYPI: pypiCatalog ?? catalog("pypi", "wheelpkg"),
    OCI: catalog("oci", "example/app"),
  };
  for (const [binding, text] of Object.entries(catalogs)) {
    const bucket = await mf.getR2Bucket(binding);
    await bucket.put("current", "g1");
    await bucket.put("generations/g1/catalog.json", text);
    await bucket.put("generations/g1/index.html", '<a href="catalog.json">catalog</a>');
    await bucket.put("generations/g1/llms.txt", "# generation");
  }
  const pypi = await mf.getR2Bucket("PYPI");
  await pypi.put("wheels/wheelpkg-1.0-py3-none-any.whl", "wheel-bytes");
  await pypi.put(
    "generations/g1/simple/wheelpkg/index.html",
    '<a href="../../wheels/wheelpkg-1.0-py3-none-any.whl">wheel</a>',
  );
  const npm = await mf.getR2Bucket("MYNPM");
  await npm.put("tarballs/abc.tgz", "tarball-bytes");
}

function worker() {
  return new Miniflare({
    modules: true,
    scriptPath: join(bundleDir, "entry.js"),
    // Miniflare 4.20260730.0 supports compatibility dates through 2026-08-06.
    compatibilityDate: "2026-08-06",
    modulesRules: [
      { type: "ESModule", include: ["**/*.js"] },
      { type: "CompiledWasm", include: ["**/*.wasm"] },
    ],
    r2Buckets: ["MYCRATES", "MYNPM", "PYPI", "OCI", "OCI_BLOBS"],
    bindings: {
      STUFF_ROUTES: ROUTES,
      STUFF_UNIFIED_HOST: "stuff.test",
      STUFF_OCI_HOST: "oci.test",
    },
  });
}

test("cold HEAD for the unified page uses a GET cache key", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const head = await mf.dispatchFetch("https://stuff.test/", { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
  const get = await mf.dispatchFetch("https://stuff.test/");
  assert.equal(get.status, 200);
  assert.match(await get.text(), /stuff/);
});

test("a registry on a newer catalog schema is reported, not fatal", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  const newer = JSON.stringify({ schema_version: 99, registry: "pypi", generation: "g1", packages: [] });
  await seed(mf, { pypiCatalog: newer });

  const page = await mf.dispatchFetch("https://stuff.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.match(html, /PyPI is not listed because its catalog could not be read: unsupported catalog schema_version: 99\./);
  assert.ok(html.includes("crate") && html.includes("package"));

  const llms = await mf.dispatchFetch("https://stuff.test/llms.txt");
  assert.equal(llms.status, 200);
  assert.match(await llms.text(), /> Note: PyPI is not listed because its catalog could not be read/);
});

test("unified llms.txt advertises the configured hosts", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const response = await mf.dispatchFetch("https://stuff.test/llms.txt");
  assert.equal(response.status, 200);
  const text = await response.text();
  assert.match(text, /Aggregate browser: https:\/\/stuff\.test\/ /);
  for (const origin of ["https://crates.test", "https://npm.test", "https://pypi.test", "https://oci.test"]) {
    assert.ok(text.includes(`${origin}/llms.txt`), `${origin}/llms.txt`);
  }
  assert.ok(!text.includes("17166969.xyz"));
});

test("the unified page and llms.txt include OCI", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const page = await mf.dispatchFetch("https://stuff.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.ok(html.includes("example/app"), "unified page lists the OCI repository");
  assert.ok(html.includes("OCI"), "unified page names the OCI registry");
  const llms = await mf.dispatchFetch("https://stuff.test/llms.txt");
  const text = await llms.text();
  assert.ok(text.includes("OCI registry"), "unified llms names the OCI registry");
  assert.ok(text.includes("- OCI: https://oci.test/llms.txt"), "unified llms links OCI");
  const catalog = await mf.dispatchFetch("https://oci.test/current/catalog.json");
  assert.equal(catalog.status, 200);
  assert.match(await catalog.text(), /"registry":"oci"/);
});

test("root pages link to objects that resolve", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const page = await mf.dispatchFetch("https://crates.test/");
  assert.equal(page.status, 200);
  const html = await page.text();
  const links = [...html.matchAll(/href="([^"]+)"/g)].map((match) => match[1]);
  assert.ok(links.length > 0);
  for (const href of links) {
    if (href.startsWith("http")) continue;
    const response = await mf.dispatchFetch(new URL(href, "https://crates.test/"));
    assert.equal(response.status, 200, `${href} should resolve`);
  }
  const alias = await mf.dispatchFetch("https://crates.test/catalog.json");
  assert.equal(alias.status, 200);
});

test("the gateway forwards the host to the cached entrypoint", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const cargo = await mf.dispatchFetch("https://crates.test/catalog.json");
  const npm = await mf.dispatchFetch("https://npm.test/catalog.json");
  assert.equal(cargo.status, 200);
  assert.equal(npm.status, 200);
  assert.match(await cargo.text(), /"registry":"cargo"/);
  assert.match(await npm.text(), /"registry":"npm"/);
});

test("pinned generation artifacts use the shared store aliases", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const wheel = await mf.dispatchFetch(
    "https://pypi.test/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl",
  );
  assert.equal(wheel.status, 200);
  assert.equal(await wheel.text(), "wheel-bytes");
  const tarball = await mf.dispatchFetch(
    "https://npm.test/generations/g1/-/tarballs/abc.tgz",
  );
  assert.equal(tarball.status, 200);
  assert.equal(await tarball.text(), "tarball-bytes");
  const missingGeneration = await mf.dispatchFetch(
    "https://pypi.test/generations/g2/wheels/wheelpkg-1.0-py3-none-any.whl",
  );
  assert.equal(missingGeneration.status, 404);
});

test("a ranged artifact request returns only the requested bytes", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const response = await mf.dispatchFetch(
    "https://pypi.test/generations/g1/wheels/wheelpkg-1.0-py3-none-any.whl",
    { headers: { range: "bytes=0-4" } },
  );
  assert.equal(response.status, 206);
  assert.equal(response.headers.get("content-range"), "bytes 0-4/11");
  assert.equal(await response.text(), "wheel");
  const missing = await mf.dispatchFetch("https://pypi.test/nope");
  assert.equal(missing.status, 404);
  assert.match(missing.headers.get("cache-control") ?? "", /max-age=60/);
});

test("a pinned project page's relative wheel link resolves", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seed(mf);
  const pageUrl = "https://pypi.test/generations/g1/simple/wheelpkg/index.html";
  const page = await mf.dispatchFetch(pageUrl);
  assert.equal(page.status, 200);
  const html = await page.text();
  const href = html.match(/href="([^"]+)"/)[1];
  const wheel = await mf.dispatchFetch(new URL(href, pageUrl));
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

async function seedParity(mf) {
  const bucket = await mf.getR2Bucket("MYCRATES");
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
  const mf = worker();
  t.after(() => mf.dispose());
  await seedParity(mf);
  const server = await startServer(site);
  t.after(() => {
    server.child.kill();
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
    const native = await fetch(`${server.base}${path}`);
    const edge = await mf.dispatchFetch(`https://crates.test${path}`);
    assert.equal(native.status, edge.status, `status for ${path}`);
    assert.equal(
      (native.headers.get("cache-control") ?? "").replace(/\s+/g, " "),
      (edge.headers.get("cache-control") ?? "").replace(/\s+/g, " "),
      `cache-control for ${path}`,
    );
  }
});

const OCI_FIXTURE = fileURLToPath(
  new URL("../../../tests/fixtures/oci/layout", import.meta.url),
);
const OCI_INDEX = "sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388";
const OCI_ARM64 = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
const OCI_AMD64 = "sha256:d4ae9ea60e8f1f455de9959f23d711014d5f97e9d85ce82a6ecc769a61d1c65c";
const OCI_REFERRER = "sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25";
const OCI_LAYER = "sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f";
const OCI_REFERRER_TYPE = "application/vnd.stuff.sbom.v1+json";

function ociDescriptor(mediaType, digest, size, extra = {}) {
  return { mediaType, digest, size, ...extra };
}

async function seedOci(mf) {
  const blobDir = join(OCI_FIXTURE, "blobs", "sha256");
  const meta = await mf.getR2Bucket("OCI");
  const blobs = await mf.getR2Bucket("OCI_BLOBS");
  await meta.put("current", "g1");
  const index = ociDescriptor("application/vnd.oci.image.index.v1+json", OCI_INDEX, 491);
  const arm64 = ociDescriptor("application/vnd.oci.image.manifest.v1+json", OCI_ARM64, 401);
  const amd64 = ociDescriptor("application/vnd.oci.image.manifest.v1+json", OCI_AMD64, 401);
  const referrer = ociDescriptor("application/vnd.oci.image.manifest.v1+json", OCI_REFERRER, 671, {
    artifactType: OCI_REFERRER_TYPE,
    annotations: { "org.opencontainers.image.created": "2026-11-01T00:00:00Z" },
  });
  const repository = JSON.stringify({
    schemaVersion: 1,
    repository: "example/app",
    tags: { stable: index, beta: arm64 },
    manifests: [index, arm64, amd64, referrer],
  });
  await meta.put("generations/g1/repositories/example/app/repository.json", repository);
  await meta.put(
    "generations/g1/repositories/example/app/referrers/sha256/576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4.json",
    JSON.stringify({
      schemaVersion: 2,
      mediaType: "application/vnd.oci.image.index.v1+json",
      manifests: [referrer],
    }),
  );
  for (const digest of [OCI_INDEX, OCI_ARM64, OCI_AMD64, OCI_REFERRER]) {
    await meta.put(
      `oci/manifests/sha256/${digest.slice("sha256:".length)}`,
      readFileSync(join(blobDir, digest.slice("sha256:".length))),
    );
  }
  for (const digest of [
    "120ab4e8b5d4dfbdda6b6ccd16c9e4320530c08b35463b414e411fb76d9e30c9",
    "1c17d23d45cae9129edb6a0f9ce247f5713ae939577e1b8f3e0b1bdf66972b55",
    "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
    "6615b2d5776183ba25e1bfa8e789346fe3a65066e808ac5b6beb5710bb94d10e",
    "882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f",
    "bf2585f7b79b5ba8a5d7f256f7c2ce5a537ebc1c0d9e043ca90c461f2183f718",
  ]) {
    await blobs.put(`sha256/${digest}`, readFileSync(join(blobDir, digest)));
  }
}

test("oci ping and manifest reads ignore Accept and pin the stored type", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seedOci(mf);

  const ping = await mf.dispatchFetch("https://oci.test/v2/");
  assert.equal(ping.status, 200);
  assert.equal(ping.headers.get("docker-distribution-api-version"), "registry/2.0");
  assert.equal(await ping.text(), "{}");

  const byTag = await mf.dispatchFetch("https://oci.test/v2/example/app/manifests/stable", {
    headers: { accept: "application/vnd.docker.distribution.manifest.list.v2+json" },
  });
  assert.equal(byTag.status, 200);
  assert.equal(byTag.headers.get("content-type"), "application/vnd.oci.image.index.v1+json");
  assert.equal(byTag.headers.get("docker-content-digest"), OCI_INDEX);
  assert.equal(byTag.headers.get("cache-control"), "public, max-age=60, stale-if-error=0");
  assert.equal((await byTag.arrayBuffer()).byteLength, 491);

  const byDigest = await mf.dispatchFetch(`https://oci.test/v2/example/app/manifests/${OCI_INDEX}`);
  assert.equal(byDigest.status, 200);
  assert.match(byDigest.headers.get("cache-control"), /immutable/);
  const head = await mf.dispatchFetch(`https://oci.test/v2/example/app/manifests/${OCI_INDEX}`, {
    method: "HEAD",
  });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
  assert.equal(head.headers.get("content-length"), "491");
});

test("oci blob reads support HEAD and a single range", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seedOci(mf);
  const base = `https://oci.test/v2/example/app/blobs/${OCI_LAYER}`;

  const full = await mf.dispatchFetch(base);
  assert.equal(full.status, 200);
  assert.equal(full.headers.get("docker-content-digest"), OCI_LAYER);
  assert.equal((await full.arrayBuffer()).byteLength, 117);

  const head = await mf.dispatchFetch(base, { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(head.headers.get("content-length"), "117");
  assert.equal(await head.text(), "");

  const ranged = await mf.dispatchFetch(base, { headers: { range: "bytes=0-9" } });
  assert.equal(ranged.status, 206);
  assert.equal(ranged.headers.get("content-range"), "bytes 0-9/117");
  const bytes = Buffer.from(await ranged.arrayBuffer());
  assert.deepEqual(
    bytes,
    readFileSync(join(OCI_FIXTURE, "blobs", "sha256", OCI_LAYER.slice("sha256:".length))).subarray(0, 10),
  );

  const missing = await mf.dispatchFetch(
    "https://oci.test/v2/example/app/blobs/sha256:1111111111111111111111111111111111111111111111111111111111111111",
  );
  assert.equal(missing.status, 404);
  assert.match(await missing.text(), /BLOB_UNKNOWN/);
});

test("oci tags paginate and referrers honor artifactType", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seedOci(mf);

  const page1 = await mf.dispatchFetch("https://oci.test/v2/example/app/tags/list?n=1");
  assert.equal(page1.status, 200);
  assert.deepEqual(await page1.json(), { name: "example/app", tags: ["beta"] });
  assert.equal(
    page1.headers.get("link"),
    '</v2/example/app/tags/list?n=1&last=beta>; rel="next"',
  );
  const page2 = await mf.dispatchFetch(
    "https://oci.test/v2/example/app/tags/list?n=1&last=beta",
  );
  assert.deepEqual(await page2.json(), { name: "example/app", tags: ["stable"] });
  assert.equal(page2.headers.get("link"), null);

  const all = await mf.dispatchFetch(`https://oci.test/v2/example/app/referrers/${OCI_ARM64}`);
  assert.equal(all.status, 200);
  const allBody = await all.json();
  assert.equal(allBody.manifests.length, 1);
  assert.equal(allBody.manifests[0].digest, OCI_REFERRER);

  const filtered = await mf.dispatchFetch(
    `https://oci.test/v2/example/app/referrers/${OCI_ARM64}?artifactType=${encodeURIComponent(OCI_REFERRER_TYPE)}`,
  );
  assert.equal(filtered.headers.get("oci-filters-applied"), "artifactType");
  assert.equal((await filtered.json()).manifests.length, 1);

  const unknown = await mf.dispatchFetch(
    `https://oci.test/v2/example/app/referrers/${OCI_ARM64}?artifactType=${encodeURIComponent("application/vnd.example.unknown")}`,
  );
  assert.deepEqual((await unknown.json()).manifests, []);
});

test("oci errors are OCI JSON with the expected status", async (t) => {
  const mf = worker();
  t.after(() => mf.dispose());
  await seedOci(mf);

  const unknownManifest = await mf.dispatchFetch(
    "https://oci.test/v2/example/app/manifests/nope",
  );
  assert.equal(unknownManifest.status, 404);
  assert.match(await unknownManifest.text(), /MANIFEST_UNKNOWN/);

  const unknownRepository = await mf.dispatchFetch("https://oci.test/v2/other/repo/tags/list");
  assert.equal(unknownRepository.status, 404);
  assert.match(await unknownRepository.text(), /NAME_UNKNOWN/);

  const badName = await mf.dispatchFetch("https://oci.test/v2/Example/app/manifests/stable");
  assert.equal(badName.status, 400);
  assert.match(await badName.text(), /NAME_INVALID/);

  const badDigest = await mf.dispatchFetch("https://oci.test/v2/example/app/blobs/sha256:00");
  assert.equal(badDigest.status, 400);
  assert.match(await badDigest.text(), /DIGEST_INVALID/);

  const write = await mf.dispatchFetch("https://oci.test/v2/example/app/blobs/uploads/", {
    method: "POST",
  });
  assert.equal(write.status, 405);
  assert.equal(write.headers.get("allow"), "GET, HEAD");
  assert.match(await write.text(), /UNSUPPORTED/);
});
