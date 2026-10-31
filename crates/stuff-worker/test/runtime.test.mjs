import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
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

async function seed(mf) {
  const catalogs = {
    MYCRATES: catalog("cargo", "crate"),
    MYNPM: catalog("npm", "package"),
    PYPI: catalog("pypi", "wheelpkg"),
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
    scriptPath: fileURLToPath(new URL("../build/worker/entry.js", import.meta.url)),
    compatibilityDate: "2025-07-18",
    modulesRules: [{ type: "CompiledWasm", include: ["**/*.wasm"] }],
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
