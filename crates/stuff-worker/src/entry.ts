import {
  initSync,
  map_asset_json,
  plan_object_response_json,
  render_unified_index_json,
  render_unified_llms_txt_json,
  resolve_npm_tarball_json,
  resolve_pointer_json,
} from "../build/wasm/stuff_worker.js";
import wasmModule from "../build/wasm/stuff_worker_bg.wasm";

initSync(wasmModule);

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
}

interface WorkerEnv {
  readonly STUFF_ROUTES: string;
  readonly STUFF_UNIFIED_HOST?: string;
  readonly [binding: string]: R2BucketLike | string | undefined;
}

interface Mapped {
  kind: "direct" | "via_pointer" | "not_found" | "error";
  bucket_binding?: string;
  key?: string;
  pointer_key?: string;
  rel_path?: string;
  message?: string;
}

interface PointerResolution {
  kind: "key" | "not_found";
  key?: string;
  alias_key?: string;
  npm_packument_key?: string;
  npm_version?: string;
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

function bucket(env: WorkerEnv, binding: string): R2BucketLike {
  const found = env[binding];
  if (!found || typeof found === "string" || typeof found.get !== "function") {
    throw new Error(`missing R2 binding: ${binding}`);
  }
  return found;
}

async function catalogText(env: WorkerEnv, binding: string): Promise<string | null> {
  const store = bucket(env, binding);
  const pointer = await store.get("current");
  if (pointer === null) {
    return null;
  }
  const id = (await pointer.text()).trim();
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(id)) {
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
  const cached = await cache.match(req);
  if (cached) {
    return cached;
  }
  const parts: string[] = [];
  for (const binding of ["MYCRATES", "MYNPM", "PYPI"]) {
    const text = await catalogText(env, binding);
    if (text === null) {
      return new Response("unified catalog not ready", { status: 404 });
    }
    parts.push(text);
  }
  const url = new URL(req.url);
  const wantMarkdown = url.pathname === "/llms.txt";
  const rendered = JSON.parse(
    wantMarkdown
      ? render_unified_llms_txt_json(JSON.stringify(parts))
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
  // immutable, so a short edge cache keeps the landing page fresh.
  await cache.put(req, response.clone());
  return response;
}

export default {
  async fetch(req: Request, env: WorkerEnv): Promise<Response> {
    if (req.method !== "GET" && req.method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }
    const url = new URL(req.url);
    const unifiedHost = env.STUFF_UNIFIED_HOST;
    if (
      typeof unifiedHost === "string" &&
      unifiedHost !== "" &&
      url.hostname === unifiedHost &&
      (url.pathname === "/" || url.pathname === "" || url.pathname === "/llms.txt")
    ) {
      if (req.method === "HEAD") {
        const res = await unifiedIndex(req, env);
        return new Response(null, { status: res.status, headers: res.headers });
      }
      return unifiedIndex(req, env);
    }
    const mapped = JSON.parse(
      map_asset_json(env.STUFF_ROUTES, url.hostname, url.pathname),
    ) as Mapped;
    if (mapped.kind === "error") {
      throw new Error(mapped.message ?? "routes");
    }
    if (mapped.kind === "not_found" || !mapped.bucket_binding) {
      return new Response("not found", { status: 404 });
    }
    let key: string;
    let route: "immutable" | "pointer";
    let aliasKey: string | undefined;
    let npmPackumentKey: string | undefined;
    let npmVersion: string | undefined;
    if (mapped.kind === "direct") {
      if (!mapped.key) {
        throw new Error("mapping");
      }
      key = mapped.key;
      route = "immutable";
    } else {
      if (!mapped.pointer_key || mapped.rel_path === undefined) {
        throw new Error("mapping");
      }
      const pointer = await bucket(env, mapped.bucket_binding).get(mapped.pointer_key);
      if (pointer === null) {
        return new Response("not found", { status: 404 });
      }
      const resolved = JSON.parse(
        resolve_pointer_json(mapped.pointer_key, await pointer.text(), mapped.rel_path),
      ) as PointerResolution;
      if (resolved.kind !== "key" || !resolved.key) {
        return new Response("not found", { status: 404 });
      }
      key = resolved.key;
      route = "pointer";
      aliasKey = resolved.alias_key;
      npmPackumentKey = resolved.npm_packument_key;
      npmVersion = resolved.npm_version;
    }
    const store = bucket(env, mapped.bucket_binding);
    let object = await store.get(key);
    // Generation-scoped immutable aliases (mypypi wheels/, mynpm tarballs/
    // and -/tarballs/) are not uploaded as separate R2 objects; the shared
    // store copy is byte-identical by the immutability invariant.
    if (object === null && aliasKey) {
      const aliased = await store.get(aliasKey);
      if (aliased !== null) {
        key = aliasKey;
        object = aliased;
      }
    }
    if (object === null && npmPackumentKey && npmVersion !== undefined && mapped.pointer_key) {
      const packument = await store.get(npmPackumentKey);
      if (packument !== null) {
        const tarball = JSON.parse(
          resolve_npm_tarball_json(mapped.pointer_key, await packument.text(), npmVersion),
        ) as PointerResolution;
        if (tarball.alias_key) {
          const aliased = await store.get(tarball.alias_key);
          if (aliased !== null) {
            key = tarball.alias_key;
            object = aliased;
            route = "immutable";
          }
        }
      }
    }
    if (object === null) {
      return new Response("not found", { status: 404 });
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
    return new Response(object.body, { status: plan.status, headers });
  },
};
