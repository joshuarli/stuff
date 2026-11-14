import {
  initSync,
  map_asset_json,
  generation_id_valid,
  oci_auth_challenge_json,
  oci_auth_check,
  oci_blob_json,
  oci_error_json,
  oci_generation_json,
  oci_manifest_json,
  oci_manifest_put_json,
  oci_manifest_refs_json,
  oci_method_not_allowed_json,
  oci_mount_json,
  oci_ping_json,
  oci_referrers_json,
  oci_request_json,
  oci_session_json,
  oci_tags_json,
  oci_write_request_json,
  plan_object_response_json,
  render_unified_index_json,
  render_unified_llms_txt_json,
  resolve_direct_alias_json,
  resolve_npm_tarball_json,
  resolve_pointer_json,
} from "./build/wasm/stuff_worker.js";
import wasmModule from "./build/wasm/stuff_worker_bg.wasm";

initSync({ module: wasmModule });

interface R2ObjectLike {
  readonly size: number;
  readonly httpEtag: string;
  readonly etag: string;
  readonly body: ReadableStream | null;
  text(): Promise<string>;
}

interface R2BucketLike {
  get(
    key: string,
    options?: { range: { offset: number; length: number } },
  ): Promise<R2ObjectLike | null>;
  head(key: string): Promise<R2ObjectLike | null>;
  put(
    key: string,
    value: string | Uint8Array,
    options?: { onlyIf?: { etagMatches?: string; etagDoesNotMatch?: string } },
  ): Promise<R2ObjectLike | null>;
  list(options?: { prefix?: string; cursor?: string }): Promise<R2ListingLike>;
}

type R2ListingLike =
  | { objects: { key: string }[]; truncated: true; cursor: string }
  | { objects: { key: string }[]; truncated: false };

interface WorkerEnv {
  readonly STUFF_ROUTES: string;
  readonly STUFF_UNIFIED_HOST?: string;
  readonly STUFF_OCI_HOST?: string;
  readonly STUFF_OCI_BLOB_ORIGIN?: string;
  readonly STUFF_OCI_PUSH_TOKEN?: string;
  readonly STUFF_OCI_PUSH_SECRET?: string;
  readonly STUFF_OCI_WRITER_ORIGIN?: string;
  readonly STUFF_RELEASE: string;
  readonly [binding: string]: R2BucketLike | string | undefined;
}

interface Mapped {
  kind: "direct" | "via_pointer" | "not_found" | "error";
  bucket_binding?: string;
  key?: string;
  key_prefix?: string;
  pointer_key?: string;
  rel_path?: string;
  fallback_rel_path?: string;
  fallback_key?: string;
  cache?: "immutable" | "pointer";
  message?: string;
}

interface PointerResolution {
  kind: "key" | "not_found";
  key?: string;
  alias_key?: string;
  generation_catalog_key?: string;
  npm_packument_key?: string;
  npm_version?: string;
}

interface DirectAlias {
  kind: "key" | "not_found";
  alias_key?: string;
  generation_catalog_key?: string;
}

interface ResponsePlan {
  kind: "plan" | "error";
  status: number;
  headers: Record<string, string>;
  body?: string;
  range_start?: number;
  range_length?: number;
  head: boolean;
  message?: string;
}

interface UnifiedRendered {
  kind: "html" | "markdown" | "error";
  body?: string;
  message?: string;
}

interface OciRequest {
  kind: string;
  repository?: string;
  reference?: string;
  digest?: string;
  subject?: string;
  artifact_type?: string;
  n?: number;
  last?: string;
  head: boolean;
  status?: number;
  body?: string;
  message?: string;
}

interface OciPlan {
  kind: string;
  status: number;
  headers: Record<string, string>;
  body?: string;
  bucket?: string;
  key?: string;
  range_start?: number;
  range_length?: number;
  head: boolean;
  message?: string;
}

interface OciWrite {
  kind: string;
  repository?: string;
  method?: string;
  reference?: string;
  digest?: string;
  mount?: string;
  status?: number;
  body?: string;
}

interface OciGenerationFile {
  key: string;
  text: string;
}

interface OciGeneration {
  kind: string;
  generation?: string;
  digest?: string;
  manifest_key?: string;
  files?: OciGenerationFile[];
  status?: number;
  body?: string;
}

function bucket(env: WorkerEnv, binding: string): R2BucketLike {
  const found = env[binding];
  if (!found || typeof found === "string" || !("get" in found) || typeof found.get !== "function") {
    throw new Error(`missing R2 binding: ${binding}`);
  }
  return found;
}

const NOT_FOUND_CACHE_CONTROL = "public, max-age=60";

function notFound(): Response {
  return new Response("not found", {
    status: 404,
    headers: { "cache-control": NOT_FOUND_CACHE_CONTROL },
  });
}

const UNIFIED_BINDINGS = ["MYCRATES", "MYNPM", "PYPI", "OCI", "MIRROR"] as const;

async function currentGeneration(env: WorkerEnv, binding: string): Promise<string | null> {
  const pointer = await bucket(env, binding).get("current");
  if (pointer === null) {
    return null;
  }
  const id = (await pointer.text()).trim();
  return generation_id_valid(id) ? id : null;
}

async function generationCatalog(
  env: WorkerEnv,
  binding: string,
  generation: string,
): Promise<string | null> {
  const object = await bucket(env, binding).get(`generations/${generation}/catalog.json`);
  return object === null ? null : object.text();
}

function unifiedNotReady(): Response {
  return new Response("unified catalog not ready", {
    status: 404,
    headers: { "cache-control": NOT_FOUND_CACHE_CONTROL },
  });
}

function renderUnified(env: WorkerEnv, url: URL, catalogs: string[]): string {
  const wantMarkdown = url.pathname === "/llms.txt";
  const rendered = JSON.parse(
    wantMarkdown
      ? render_unified_llms_txt_json(JSON.stringify(catalogs), env.STUFF_ROUTES, url.origin)
      : render_unified_index_json(JSON.stringify(catalogs), env.STUFF_ROUTES, url.origin),
  ) as UnifiedRendered;
  if (rendered.kind !== (wantMarkdown ? "markdown" : "html") || !rendered.body) {
    throw new Error(rendered.message ?? "unified render");
  }
  return rendered.body;
}

const RENDER_CACHE_ENTRIES = 64;
const RENDER_CACHE_BYTES = 16 * 1024 * 1024;
const renderedPages = new Map<string, { body: string; size: number }>();
let renderedPageBytes = 0;

async function unifiedIndex(req: Request, env: WorkerEnv): Promise<Response> {
  const url = new URL(req.url);
  const generations = await Promise.all(
    UNIFIED_BINDINGS.map((binding) => currentGeneration(env, binding)),
  );
  if (generations.some((generation) => generation === null)) {
    return unifiedNotReady();
  }
  const ids = generations as string[];
  const headers = new Headers({
    "content-type":
      url.pathname === "/llms.txt" ? "text/markdown; charset=utf-8" : "text/html; charset=utf-8",
    "cache-control": "public, max-age=60",
  });
  const renderKey = JSON.stringify([url.origin, env.STUFF_RELEASE, ids, url.pathname]);
  const cached = renderedPages.get(renderKey);
  if (cached !== undefined) {
    renderedPages.delete(renderKey);
    renderedPages.set(renderKey, cached);
    return new Response(req.method === "HEAD" ? null : cached.body, { status: 200, headers });
  }
  const catalogs = await Promise.all(
    UNIFIED_BINDINGS.map((binding, index) => generationCatalog(env, binding, ids[index] ?? "")),
  );
  if (catalogs.some((text) => text === null)) {
    return unifiedNotReady();
  }
  const body = renderUnified(env, url, catalogs as string[]);
  const size = new TextEncoder().encode(body).byteLength;
  if (size <= RENDER_CACHE_BYTES) {
    while (renderedPages.size >= RENDER_CACHE_ENTRIES || renderedPageBytes + size > RENDER_CACHE_BYTES) {
      const oldest = renderedPages.keys().next().value;
      if (oldest === undefined) break;
      renderedPageBytes -= renderedPages.get(oldest)!.size;
      renderedPages.delete(oldest);
    }
    renderedPages.set(renderKey, { body, size });
    renderedPageBytes += size;
  }
  return new Response(req.method === "HEAD" ? null : body, { status: 200, headers });
}

function ociPlanResponse(plan: OciPlan): Response {
  if (plan.kind !== "plan") {
    throw new Error(plan.message ?? "oci plan");
  }
  return new Response(plan.body ?? null, {
    status: plan.status,
    headers: new Headers(plan.headers),
  });
}

async function sendOciPlan(
  plan: OciPlan,
  store: R2BucketLike,
  key: string,
): Promise<Response> {
  if (plan.kind !== "plan") {
    throw new Error(plan.message ?? "oci plan");
  }
  const headers = new Headers(plan.headers);
  if (plan.body !== undefined) {
    return new Response(plan.body, { status: plan.status, headers });
  }
  if (plan.status === 304 || plan.status === 416 || plan.head) {
    return new Response(null, { status: plan.status, headers });
  }
  if (plan.range_start !== undefined && plan.range_length !== undefined) {
    const ranged = await store.get(key, {
      range: { offset: plan.range_start, length: plan.range_length },
    });
    if (ranged === null) {
      throw new Error("oci object vanished");
    }
    return new Response(ranged.body, { status: plan.status, headers });
  }
  const object = await store.get(key);
  if (object === null) {
    throw new Error("oci object vanished");
  }
  return new Response(object.body, { status: plan.status, headers });
}

function writeErrorPlan(status: number | undefined, body: string | undefined): OciPlan {
  const text = body ?? "";
  return {
    kind: "plan",
    status: status ?? 400,
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
      "content-length": String(text.length),
    },
    body: text,
    head: false,
  };
}

async function ociPushManifest(
  req: Request,
  env: WorkerEnv,
  url: URL,
  meta: R2BucketLike,
  repository: string,
  reference: string,
): Promise<Response> {
  const length = Number(req.headers.get("content-length") ?? "0");
  if (!Number.isFinite(length) || length > 4 * 1024 * 1024) {
    return ociPlanResponse(
      writeErrorPlan(
        400,
        JSON.stringify({
          errors: [{ code: "SIZE_INVALID", message: "manifest body is too large" }],
        }),
      ),
    );
  }
  const body = await req.text();
  const refs = JSON.parse(oci_manifest_refs_json(body)) as {
    kind: string;
    digest?: string;
    blobs?: string[];
    children?: string[];
    status?: number;
    body?: string;
  };
  if (refs.kind !== "refs") {
    return ociPlanResponse(writeErrorPlan(refs.status, refs.body));
  }
  const blobs = bucket(env, "OCI_BLOBS");
  const availableBlobs: string[] = [];
  for (const digest of refs.blobs ?? []) {
    if (digest.startsWith("sha256:")) {
      const object = await blobs.head(`sha256/${digest.slice("sha256:".length)}`);
      if (object === null) {
        return ociPlanResponse(
          JSON.parse(oci_error_json("MANIFEST_BLOB_UNKNOWN", digest)) as OciPlan,
        );
      }
    }
    availableBlobs.push(digest);
  }
  const availableChildren: string[] = [];
  for (const digest of refs.children ?? []) {
    if (digest.startsWith("sha256:")) {
      const object = await meta.head(`oci/manifests/sha256/${digest.slice("sha256:".length)}`);
      if (object === null) {
        return ociPlanResponse(
          JSON.parse(oci_error_json("MANIFEST_BLOB_UNKNOWN", digest)) as OciPlan,
        );
      }
    }
    availableChildren.push(digest);
  }
  const publicUrl = `${url.protocol}//${url.host}`;
  const encoder = new TextEncoder();
  for (let attempt = 0; attempt < 5; attempt += 1) {
    const pointer = await meta.get("current");
    const snapshot =
      pointer === null ? EMPTY_OCI_SNAPSHOT : await ociSnapshot(meta, (await pointer.text()).trim());
    const planned = JSON.parse(
      oci_generation_json(
        repository,
        JSON.stringify(snapshot.repositories),
        snapshot.blobsText,
        JSON.stringify(snapshot.referrers),
        body,
        reference,
        JSON.stringify(availableBlobs),
        JSON.stringify(availableChildren),
        publicUrl,
      ),
    ) as OciGeneration;
    if (planned.kind !== "ok" || !planned.generation || !planned.digest || !planned.manifest_key) {
      return ociPlanResponse(writeErrorPlan(planned.status, planned.body));
    }
    const generation = planned.generation;
    const objects: { key: string; size: number; etag: string }[] = [];
    objects.push({
      key: planned.manifest_key,
      size: encoder.encode(body).length,
      etag: written(await meta.put(planned.manifest_key, body), planned.manifest_key).etag,
    });
    for (const file of planned.files ?? []) {
      const key = `generations/${generation}/${file.key}`;
      const value = encoder.encode(file.text);
      objects.push({ key, size: value.length, etag: written(await meta.put(key, value), key).etag });
    }
    const manifestKey = `manifests/${generation}.json`;
    written(
      await meta.put(manifestKey, JSON.stringify({ schemaVersion: 1, objects })),
      manifestKey,
    );
    const activated = await meta.put("current", generation, {
      onlyIf:
        pointer === null ? { etagDoesNotMatch: "*" } : { etagMatches: pointer.etag },
    });
    if (activated !== null) {
      return ociPlanResponse(
        JSON.parse(oci_manifest_put_json(repository, planned.digest)) as OciPlan,
      );
    }
  }
  return ociPlanResponse(
    JSON.parse(oci_error_json("CONFLICT", "could not activate the generation")) as OciPlan,
  );
}

function written(object: R2ObjectLike | null, key: string): R2ObjectLike {
  if (object === null) {
    throw new Error(`unconditional R2 write returned no object: ${key}`);
  }
  return object;
}

interface OciSnapshot {
  readonly repositories: { name: string; text: string }[];
  readonly blobsText: string;
  readonly referrers: { repository: string; subject: string; text: string }[];
}

const EMPTY_OCI_SNAPSHOT: OciSnapshot = { repositories: [], blobsText: "", referrers: [] };

async function listAll(store: R2BucketLike, prefix: string): Promise<string[]> {
  const keys: string[] = [];
  let cursor: string | undefined;
  for (;;) {
    const listed = await store.list(cursor === undefined ? { prefix } : { prefix, cursor });
    for (const object of listed.objects) {
      keys.push(object.key);
    }
    if (!listed.truncated) {
      return keys;
    }
    cursor = listed.cursor;
  }
}

async function requiredText(store: R2BucketLike, key: string): Promise<string> {
  const object = await store.get(key);
  if (object === null) {
    throw new Error(`oci generation object is missing: ${key}`);
  }
  return object.text();
}

const REFERRERS_INFIX = "/referrers/sha256/";

async function ociSnapshot(meta: R2BucketLike, generation: string): Promise<OciSnapshot> {
  if (!generation_id_valid(generation)) {
    throw new Error("invalid oci generation pointer");
  }
  const prefix = `generations/${generation}/repositories/`;
  const repositories: { name: string; text: string }[] = [];
  const referrers: { repository: string; subject: string; text: string }[] = [];
  for (const key of await listAll(meta, prefix)) {
    const path = key.slice(prefix.length);
    if (path.endsWith("/repository.json")) {
      const name = path.slice(0, -"/repository.json".length);
      repositories.push({ name, text: await requiredText(meta, key) });
      continue;
    }
    const infix = path.lastIndexOf(REFERRERS_INFIX);
    if (infix > 0 && path.endsWith(".json")) {
      referrers.push({
        repository: path.slice(0, infix),
        subject: `sha256:${path.slice(infix + REFERRERS_INFIX.length, -".json".length)}`,
        text: await requiredText(meta, key),
      });
      continue;
    }
    throw new Error(`unexpected oci generation object: ${key}`);
  }
  const blobsText = await requiredText(meta, `generations/${generation}/blobs.json`);
  return { repositories, blobsText, referrers };
}

async function ociPush(req: Request, env: WorkerEnv, url: URL): Promise<Response> {
  const parsed = JSON.parse(
    oci_write_request_json(req.method, `${url.pathname}${url.search}`),
  ) as OciWrite;
  if (parsed.kind === "error") {
    return ociPlanResponse(writeErrorPlan(parsed.status, parsed.body));
  }
  if (parsed.kind === "unsupported") {
    return ociPlanResponse(JSON.parse(oci_method_not_allowed_json()) as OciPlan);
  }
  if (parsed.kind !== "write") {
    return ociPlanResponse(
      JSON.parse(oci_error_json("NAME_UNKNOWN", "unknown path")) as OciPlan,
    );
  }
  const token = env.STUFF_OCI_PUSH_TOKEN ?? "";
  const authorization = req.headers.get("authorization") ?? "";
  if (token === "" || !oci_auth_check(authorization, token)) {
    return ociPlanResponse(JSON.parse(oci_auth_challenge_json()) as OciPlan);
  }
  const repository = parsed.repository ?? "";
  if (parsed.method === "start" || parsed.method === "mount") {
    if (parsed.method === "mount" && parsed.mount?.startsWith("sha256:")) {
      const object = await bucket(env, "OCI_BLOBS").head(
        `sha256/${parsed.mount.slice("sha256:".length)}`,
      );
      if (object !== null) {
        return ociPlanResponse(
          JSON.parse(oci_mount_json(repository, parsed.mount)) as OciPlan,
        );
      }
    }
    const writer = env.STUFF_OCI_WRITER_ORIGIN ?? "";
    const secret = env.STUFF_OCI_PUSH_SECRET ?? "";
    if (writer === "" || secret === "") {
      throw new Error("OCI push writer is not configured");
    }
    const uploadId = crypto.randomUUID().replace(/-/g, "");
    const now = BigInt(Math.floor(Date.now() / 1000));
    return ociPlanResponse(
      JSON.parse(oci_session_json(repository, writer, secret, now, uploadId)) as OciPlan,
    );
  }
  if (parsed.method === "manifest_put") {
    return ociPushManifest(
      req,
      env,
      url,
      bucket(env, "OCI"),
      repository,
      parsed.reference ?? "",
    );
  }
  return ociPlanResponse(
    JSON.parse(oci_error_json("BLOB_UNKNOWN", "upload sessions live on the writer")) as OciPlan,
  );
}

async function ociFetch(req: Request, env: WorkerEnv, url: URL): Promise<Response> {
  if (
    req.method !== "GET" &&
    req.method !== "HEAD" &&
    (env.STUFF_OCI_PUSH_TOKEN ?? "") !== ""
  ) {
    return ociPush(req, env, url);
  }
  const parsed = JSON.parse(
    oci_request_json(req.method, `${url.pathname}${url.search}`),
  ) as OciRequest;
  if (parsed.kind === "method_not_allowed") {
    return ociPlanResponse(JSON.parse(oci_method_not_allowed_json()) as OciPlan);
  }
  if (parsed.kind === "not_found") {
    return ociPlanResponse(
      JSON.parse(oci_error_json("NAME_UNKNOWN", "unknown path")) as OciPlan,
    );
  }
  if (parsed.kind === "error") {
    const body = parsed.body ?? "";
    return new Response(body, {
      status: parsed.status ?? 400,
      headers: {
        "content-type": "application/json",
        "content-length": String(body.length),
        "cache-control": "no-store",
      },
    });
  }
  if (parsed.kind === "ping") {
    return ociPlanResponse(JSON.parse(oci_ping_json()) as OciPlan);
  }
  if (parsed.kind === "blob") {
    const digest = parsed.digest ?? "";
    if (!digest.startsWith("sha256:")) {
      return ociPlanResponse(
        JSON.parse(oci_error_json("DIGEST_INVALID", digest)) as OciPlan,
      );
    }
    const blobOrigin = env.STUFF_OCI_BLOB_ORIGIN;
    if (typeof blobOrigin === "string" && blobOrigin !== "") {
      const location = `${blobOrigin.replace(/\/+$/, "")}/sha256/${digest.slice("sha256:".length)}`;
      return new Response(null, {
        status: 307,
        headers: {
          location,
          "docker-content-digest": digest,
          "cache-control": "public, max-age=31536000, immutable",
        },
      });
    }
    const store = bucket(env, "OCI_BLOBS");
    const key = `sha256/${digest.slice("sha256:".length)}`;
    const object = await store.head(key);
    if (object === null) {
      return ociPlanResponse(
        JSON.parse(oci_error_json("BLOB_UNKNOWN", digest)) as OciPlan,
      );
    }
    const plan = JSON.parse(
      oci_blob_json(
        req.method,
        digest,
        BigInt(object.size),
        JSON.stringify({ range: req.headers.get("range") }),
      ),
    ) as OciPlan;
    return sendOciPlan(plan, store, key);
  }
  const repository = parsed.repository ?? "";
  const meta = bucket(env, "OCI");
  const pointer = await meta.get("current");
  if (pointer === null) {
    return ociPlanResponse(
      JSON.parse(oci_error_json("NAME_UNKNOWN", repository)) as OciPlan,
    );
  }
  const generation = (await pointer.text()).trim();
  if (!generation_id_valid(generation)) {
    throw new Error("invalid oci generation pointer");
  }
  const repositoryKey = `generations/${generation}/repositories/${repository}/repository.json`;
  const repositoryObject = await meta.get(repositoryKey);
  if (repositoryObject === null) {
    return ociPlanResponse(
      JSON.parse(oci_error_json("NAME_UNKNOWN", repository)) as OciPlan,
    );
  }
  const repositoryText = await repositoryObject.text();
  if (parsed.kind === "tags") {
    const plan = JSON.parse(
      oci_tags_json(repository, repositoryText, parsed.n, parsed.last),
    ) as OciPlan;
    return ociPlanResponse(plan);
  }
  if (parsed.kind === "referrers") {
    const subject = parsed.subject ?? "";
    const referrersKey = `generations/${generation}/repositories/${repository}/referrers/sha256/${subject.slice("sha256:".length)}.json`;
    const referrersObject = await meta.get(referrersKey);
    const referrersText = referrersObject === null ? "" : await referrersObject.text();
    const plan = JSON.parse(
      oci_referrers_json(
        repository,
        subject,
        referrersText,
        parsed.artifact_type,
        parsed.n,
        parsed.last,
      ),
    ) as OciPlan;
    return ociPlanResponse(plan);
  }
  if (parsed.kind === "manifest") {
    const reference = parsed.reference ?? "";
    const plan = JSON.parse(
      oci_manifest_json(repository, repositoryText, reference, parsed.head),
    ) as OciPlan;
    if (plan.kind !== "plan" || plan.body !== undefined) {
      return ociPlanResponse(plan);
    }
    const key = plan.key ?? "";
    const object = parsed.head ? await meta.head(key) : await meta.get(key);
    if (object === null) {
      return ociPlanResponse(
        JSON.parse(oci_error_json("MANIFEST_UNKNOWN", reference)) as OciPlan,
      );
    }
    const headers = new Headers(plan.headers);
    return parsed.head
      ? new Response(null, { status: plan.status, headers })
      : new Response(object.body, { status: plan.status, headers });
  }
  throw new Error(`unknown oci request: ${parsed.kind}`);
}

interface PrimaryRead {
  readonly generation: R2ObjectLike | null;
  readonly object: R2ObjectLike | null;
}

const lastPointerText = new Map<string, string>();

async function primaryRead(
  store: R2BucketLike,
  fetchObject: (key: string) => Promise<R2ObjectLike | null>,
  key: string,
  aliasKey: string | undefined,
  generationCatalogKey: string | undefined,
): Promise<PrimaryRead> {
  if (aliasKey === undefined) {
    return { generation: null, object: await fetchObject(key) };
  }
  const [generation, object] = await Promise.all([
    generationCatalogKey === undefined ? Promise.resolve(null) : store.head(generationCatalogKey),
    fetchObject(aliasKey),
  ]);
  return { generation, object };
}

export default {
  async fetch(req: Request, env: WorkerEnv): Promise<Response> {
    const url = new URL(req.url);
    const ociHost = env.STUFF_OCI_HOST;
    if (
      typeof ociHost === "string" &&
      ociHost !== "" &&
      url.hostname === ociHost &&
      (url.pathname === "/v2" ||
        url.pathname === "/v2/" ||
        url.pathname.startsWith("/v2/"))
    ) {
      return ociFetch(req, env, url);
    }
    if (req.method !== "GET" && req.method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }
    const unifiedHost = env.STUFF_UNIFIED_HOST;
    if (
      typeof unifiedHost === "string" &&
      unifiedHost !== "" &&
      url.hostname === unifiedHost &&
      (url.pathname === "/" || url.pathname === "" || url.pathname === "/llms.txt")
    ) {
      return unifiedIndex(req, env);
    }
    const mapped = JSON.parse(
      map_asset_json(env.STUFF_ROUTES, url.hostname, url.pathname),
    ) as Mapped;
    if (mapped.kind === "error") {
      throw new Error(mapped.message ?? "routes");
    }
    if (mapped.kind === "not_found" || !mapped.bucket_binding) {
      return notFound();
    }
    const store = bucket(env, mapped.bucket_binding);
    const wantsRange = req.headers.get("range") !== null;
    const fetchObject = (key: string): Promise<R2ObjectLike | null> =>
      wantsRange ? store.head(key) : store.get(key);
    let key: string;
    let route: "immutable" | "pointer";
    let aliasKey: string | undefined;
    let generationCatalogKey: string | undefined;
    let npmPackumentKey: string | undefined;
    let npmVersion: string | undefined;
    let pointerText: string | undefined;
    let speculativeRead: Promise<PrimaryRead> | undefined;
    if (mapped.kind === "direct") {
      if (!mapped.key) {
        throw new Error("mapping");
      }
      key = mapped.key;
      route = mapped.cache ?? "immutable";
      const direct = JSON.parse(
        resolve_direct_alias_json(mapped.key_prefix ?? "", key),
      ) as DirectAlias;
      if (direct.kind === "key") {
        aliasKey = direct.alias_key;
        generationCatalogKey = direct.generation_catalog_key;
      }
    } else {
      const pointerKey = mapped.pointer_key;
      const relPath = mapped.rel_path;
      if (!pointerKey || relPath === undefined) {
        throw new Error("mapping");
      }
      const resolve = (text: string): PointerResolution =>
        JSON.parse(resolve_pointer_json(pointerKey, text, relPath)) as PointerResolution;
      const rememberedKey = `${mapped.bucket_binding}/${pointerKey}`;
      const remembered = lastPointerText.get(rememberedKey);
      if (remembered !== undefined) {
        const guess = resolve(remembered);
        if (guess.kind === "key" && guess.key) {
          speculativeRead = primaryRead(
            store,
            fetchObject,
            guess.key,
            guess.alias_key,
            guess.generation_catalog_key,
          );
          speculativeRead.catch(() => undefined);
        }
      }
      const pointer = await store.get(pointerKey);
      if (pointer === null) {
        lastPointerText.delete(rememberedKey);
        return notFound();
      }
      pointerText = await pointer.text();
      lastPointerText.set(rememberedKey, pointerText);
      if (remembered !== pointerText) {
        speculativeRead = undefined;
      }
      const resolved = resolve(pointerText);
      if (resolved.kind !== "key" || !resolved.key) {
        return notFound();
      }
      key = resolved.key;
      route = mapped.cache ?? "pointer";
      aliasKey = resolved.alias_key;
      generationCatalogKey = resolved.generation_catalog_key;
      npmPackumentKey = resolved.npm_packument_key;
      npmVersion = resolved.npm_version;
    }
    let object: R2ObjectLike | null = null;
    // Generation-scoped immutable aliases (mypypi wheels/, mynpm tarballs/
    // and -/tarballs/) are not uploaded as separate R2 objects; the shared
    // store copy is byte-identical by the immutability invariant. Validate
    // the generation exists before serving its shared artifact.
    const first = await (speculativeRead ??
      primaryRead(store, fetchObject, key, aliasKey, generationCatalogKey));
    if (aliasKey === undefined) {
      object = first.object;
    } else {
      if (generationCatalogKey !== undefined && first.generation === null) {
        return notFound();
      }
      if (first.object !== null) {
        key = aliasKey;
        object = first.object;
        route = "immutable";
      }
    }
    if (object === null && npmPackumentKey && npmVersion !== undefined && mapped.pointer_key) {
      const packument = await store.get(npmPackumentKey);
      if (packument !== null) {
        const tarball = JSON.parse(
          resolve_npm_tarball_json(mapped.pointer_key, await packument.text(), npmVersion),
        ) as PointerResolution;
        if (tarball.alias_key) {
          const aliased = await fetchObject(tarball.alias_key);
          if (aliased !== null) {
            key = tarball.alias_key;
            object = aliased;
            route = "immutable";
          }
        }
      }
    }
    if (object === null && mapped.kind === "direct" && mapped.fallback_key) {
      object = await fetchObject(mapped.fallback_key);
      if (object !== null) {
        key = mapped.fallback_key;
      }
    }
    if (
      object === null &&
      mapped.kind === "via_pointer" &&
      mapped.pointer_key &&
      pointerText !== undefined &&
      mapped.fallback_rel_path !== undefined
    ) {
      const fallback = JSON.parse(
        resolve_pointer_json(mapped.pointer_key, pointerText, mapped.fallback_rel_path),
      ) as PointerResolution;
      if (fallback.kind === "key" && fallback.key) {
        const indexed = await fetchObject(fallback.key);
        if (indexed !== null) {
          key = fallback.key;
          object = indexed;
        }
      }
    }
    if (object === null) {
      return notFound();
    }
    const plan = JSON.parse(
      plan_object_response_json(
        route,
        req.method,
        key,
        JSON.stringify({
          if_none_match: req.headers.get("if-none-match"),
          if_match: req.headers.get("if-match"),
          range: req.headers.get("range"),
        }),
        BigInt(object.size),
        object.httpEtag,
      ),
    ) as ResponsePlan;
    if (plan.kind !== "plan") {
      throw new Error(plan.message ?? "plan");
    }
    const headers = new Headers(plan.headers);
    if (plan.body !== undefined) {
      return new Response(plan.body, { status: plan.status, headers });
    }
    if (plan.status === 304 || plan.status === 416) {
      return new Response(null, { status: plan.status, headers });
    }
    if (plan.head) {
      return new Response(null, { status: plan.status, headers });
    }
    if (plan.range_start !== undefined && plan.range_length !== undefined) {
      const ranged = await store.get(key, {
        range: { offset: plan.range_start, length: plan.range_length },
      });
      if (ranged === null) {
        throw new Error("object vanished");
      }
      return new Response(ranged.body, { status: plan.status, headers });
    }
    if (wantsRange) {
      const full = await store.get(key);
      if (full === null) {
        throw new Error("object vanished");
      }
      object = full;
    }
    return new Response(object.body, { status: plan.status, headers });
  }
};
