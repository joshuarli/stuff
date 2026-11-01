import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync, spawn } from "node:child_process";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { Miniflare } from "miniflare";

const ROUTES = JSON.stringify([
  { host: "crates.test", bucket_binding: "MYCRATES", key_prefix: "" },
  { host: "npm.test", bucket_binding: "MYNPM", key_prefix: "" },
  { host: "pypi.test", bucket_binding: "PYPI", key_prefix: "" },
]);

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
    scriptPath: fileURLToPath(
      new URL(
        "../.cloudflare/output/v0/workers/default/bundle/src/entry.js",
        import.meta.url,
      ),
    ),
    // Miniflare 4.20260730.0 supports compatibility dates through 2026-08-06.
    compatibilityDate: "2026-08-06",
    modulesRules: [
      { type: "ESModule", include: ["**/*.js"] },
      { type: "CompiledWasm", include: ["**/*.wasm"] },
    ],
    r2Buckets: ["MYCRATES", "MYNPM", "PYPI"],
    bindings: {
      STUFF_ROUTES: ROUTES,
      STUFF_UNIFIED_HOST: "stuff.test",
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
  for (const origin of ["https://crates.test", "https://npm.test", "https://pypi.test"]) {
    assert.ok(text.includes(`${origin}/llms.txt`), `${origin}/llms.txt`);
  }
  assert.ok(!text.includes("17166969.xyz"));
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
