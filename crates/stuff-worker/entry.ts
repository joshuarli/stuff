import { WorkerEntrypoint } from "cloudflare:workers";
import {
  initSync,
  map_asset_json,
  generation_id_valid,
  oci_blob_json,
  oci_error_json,
  oci_manifest_json,
  oci_method_not_allowed_json,
  oci_ping_json,
  oci_referrers_json,
  oci_request_json,
  oci_tags_json,
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
  readonly body: ReadableStream | null;
  text(): Promise<string>;
}

interface R2BucketLike {
  get(
    key: string,
    options?: { range: { offset: number; length: number } },
  ): Promise<R2ObjectLike | null>;
  head(key: string): Promise<R2ObjectLike | null>;
}

interface WorkerEnv {
  readonly STUFF_ROUTES: string;
  readonly STUFF_UNIFIED_HOST?: string;
  readonly STUFF_OCI_HOST?: string;
  readonly [binding: string]: R2BucketLike | string | undefined;
}

interface WorkerContext {
  readonly exports: {
    readonly Registry: {
      fetch(request: Request, options: { cf: { cacheKey: string } }): Promise<Response>;
    };
  };
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

function bucket(env: WorkerEnv, binding: string): R2BucketLike {
  const found = env[binding];
  if (!found || typeof found === "string" || typeof found.get !== "function") {
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

async function catalogText(env: WorkerEnv, binding: string): Promise<string | null> {
  const store = bucket(env, binding);
  const pointer = await store.get("current");
  if (pointer === null) {
    return null;
  }
  const id = (await pointer.text()).trim();
  if (!generation_id_valid(id)) {
    return null;
  }
  const object = await store.get(`generations/${id}/catalog.json`);
  if (object === null) {
    return null;
  }
  return object.text();
}

async function unifiedIndex(req: Request, env: WorkerEnv): Promise<Response> {
  const cache = caches.default;
  const url = new URL(req.url);
  const cacheKey = new Request(`${url.origin}${url.pathname}`, { method: "GET" });
  const cached = await cache.match(cacheKey);
  if (cached) {
    return req.method === "HEAD"
      ? new Response(null, { status: cached.status, headers: cached.headers })
      : cached;
  }
  const texts = await Promise.all(
    ["MYCRATES", "MYNPM", "PYPI", "OCI"].map((binding) => catalogText(env, binding)),
  );
  if (texts.some((text) => text === null)) {
    return new Response("unified catalog not ready", {
      status: 404,
      headers: { "cache-control": NOT_FOUND_CACHE_CONTROL },
    });
  }
  const parts = texts as string[];
  const wantMarkdown = url.pathname === "/llms.txt";
  const rendered = JSON.parse(
    wantMarkdown
      ? render_unified_llms_txt_json(JSON.stringify(parts), env.STUFF_ROUTES, url.origin)
      : render_unified_index_json(JSON.stringify(parts)),
  ) as UnifiedRendered;
  const wantKind = wantMarkdown ? "markdown" : "html";
  if (rendered.kind !== wantKind || !rendered.body) {
    throw new Error(rendered.message ?? "unified render");
  }
  const headers = new Headers({
    "content-type": wantMarkdown ? "text/markdown; charset=utf-8" : "text/html; charset=utf-8",
    "cache-control": "public, max-age=60",
  });
  const response = new Response(rendered.body, { status: 200, headers });
  // Cache only the successful aggregate; per-generation catalogs stay
  // immutable, so a short edge cache keeps the landing page fresh. HEAD uses
  // the same GET cache key because the Cache API rejects non-GET puts.
  await cache.put(cacheKey, response.clone());
  return req.method === "HEAD"
    ? new Response(null, { status: 200, headers })
    : response;
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

async function ociFetch(req: Request, env: WorkerEnv, url: URL): Promise<Response> {
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

export class Registry extends WorkerEntrypoint<WorkerEnv> {
  async fetch(req: Request): Promise<Response> {
    const env = this.env;
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
    let key: string;
    let route: "immutable" | "pointer";
    let aliasKey: string | undefined;
    let generationCatalogKey: string | undefined;
    let npmPackumentKey: string | undefined;
    let npmVersion: string | undefined;
    let pointerText: string | undefined;
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
      if (!mapped.pointer_key || mapped.rel_path === undefined) {
        throw new Error("mapping");
      }
      const pointer = await bucket(env, mapped.bucket_binding).get(mapped.pointer_key);
      if (pointer === null) {
        return notFound();
      }
      pointerText = await pointer.text();
      const resolved = JSON.parse(
        resolve_pointer_json(mapped.pointer_key, pointerText, mapped.rel_path),
      ) as PointerResolution;
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
    const store = bucket(env, mapped.bucket_binding);
    const wantsRange = req.headers.get("range") !== null;
    const fetchObject = (key: string): Promise<R2ObjectLike | null> =>
      wantsRange ? store.head(key) : store.get(key);
    let object: R2ObjectLike | null = null;
    // Generation-scoped immutable aliases (mypypi wheels/, mynpm tarballs/
    // and -/tarballs/) are not uploaded as separate R2 objects; the shared
    // store copy is byte-identical by the immutability invariant. Validate
    // the generation exists before serving its shared artifact.
    if (aliasKey === undefined) {
      object = await fetchObject(key);
    } else {
      const [generation, aliased] = await Promise.all([
        generationCatalogKey === undefined
          ? Promise.resolve(null)
          : store.head(generationCatalogKey),
        fetchObject(aliasKey),
      ]);
      if (generationCatalogKey !== undefined && generation === null) {
        return notFound();
      }
      if (aliased !== null) {
        key = aliasKey;
        object = aliased;
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
    if (plan.head || object.body === null) {
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
    if (object.body === null) {
      const full = await store.get(key);
      if (full === null) {
        throw new Error("object vanished");
      }
      object = full;
    }
    return new Response(object.body, { status: plan.status, headers });
  }
}

export default {
  async fetch(req: Request, _env: WorkerEnv, ctx: WorkerContext): Promise<Response> {
    const url = new URL(req.url);
    return ctx.exports.Registry.fetch(req, {
      cf: { cacheKey: `${url.hostname}${url.pathname}` },
    });
  },
};
