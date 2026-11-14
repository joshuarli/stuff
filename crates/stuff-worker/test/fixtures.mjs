import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

export function catalog(registry, name) {
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

function mirrorCatalog() {
  return JSON.stringify({
    artifacts: [],
    baseUrl: "https://mirror.test/",
    generation: "g1",
    packages: [
      { name: "tool-1.0", source: "upstream", versions: [{ version: "1.0" }] },
    ],
    registry: "mirror",
    renderer: 1,
    schemaVersion: 1,
  });
}

export async function seed(server, { pypiCatalog } = {}) {
  const catalogs = {
    MYCRATES: catalog("cargo", "crate"),
    MYNPM: catalog("npm", "package"),
    PYPI: pypiCatalog ?? catalog("pypi", "wheelpkg"),
    OCI: catalog("oci", "example/app"),
    MIRROR: mirrorCatalog(),
  };
  for (const [binding, text] of Object.entries(catalogs)) {
    const bucket = (await server.getWorker().getEnv())[binding];
    await bucket.put("current", "g1");
    await bucket.put("generations/g1/catalog.json", text);
    await bucket.put("generations/g1/index.html", '<a href="catalog.json">catalog</a>');
    await bucket.put("generations/g1/llms.txt", "# generation");
  }
  const pypi = (await server.getWorker().getEnv())["PYPI"];
  await pypi.put("wheels/wheelpkg-1.0-py3-none-any.whl", "wheel-bytes");
  await pypi.put(
    "generations/g1/simple/wheelpkg/index.html",
    '<a href="../../wheels/wheelpkg-1.0-py3-none-any.whl">wheel</a>',
  );
  const npm = (await server.getWorker().getEnv())["MYNPM"];
  await npm.put("tarballs/abc.tgz", "tarball-bytes");
}

export const OCI_FIXTURE = fileURLToPath(
  new URL("../../../tests/fixtures/oci/layout", import.meta.url),
);
export const OCI_INDEX = "sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388";
export const OCI_ARM64 = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
export const OCI_AMD64 = "sha256:d4ae9ea60e8f1f455de9959f23d711014d5f97e9d85ce82a6ecc769a61d1c65c";
export const OCI_REFERRER = "sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25";
export const OCI_LAYER = "sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f";
export const OCI_REFERRER_TYPE = "application/vnd.stuff.sbom.v1+json";

export const OCI_BLOBS = [
  "120ab4e8b5d4dfbdda6b6ccd16c9e4320530c08b35463b414e411fb76d9e30c9",
  "1c17d23d45cae9129edb6a0f9ce247f5713ae939577e1b8f3e0b1bdf66972b55",
  "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
  "6615b2d5776183ba25e1bfa8e789346fe3a65066e808ac5b6beb5710bb94d10e",
  "882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f",
  "bf2585f7b79b5ba8a5d7f256f7c2ce5a537ebc1c0d9e043ca90c461f2183f718",
].map((hex) => `sha256:${hex}`);

function ociDescriptor(mediaType, digest, size, extra = {}) {
  return { mediaType, digest, size, ...extra };
}

export async function seedOci(server) {
  const blobDir = join(OCI_FIXTURE, "blobs", "sha256");
  const meta = (await server.getWorker().getEnv())["OCI"];
  const blobs = (await server.getWorker().getEnv())["OCI_BLOBS"];
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
  await meta.put("generations/g1/blobs.json", JSON.stringify({ schemaVersion: 1, blobs: OCI_BLOBS }));
  for (const digest of [OCI_INDEX, OCI_ARM64, OCI_AMD64, OCI_REFERRER]) {
    await meta.put(
      `oci/manifests/sha256/${digest.slice("sha256:".length)}`,
      readFileSync(join(blobDir, digest.slice("sha256:".length))),
    );
  }
  for (const digest of OCI_BLOBS) {
    const hex = digest.slice("sha256:".length);
    await blobs.put(`sha256/${hex}`, readFileSync(join(blobDir, hex)));
  }
}
