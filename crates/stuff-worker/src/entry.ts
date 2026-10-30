import {
  map_asset_json,
  plan_object_response_json,
  resolve_pointer_json,
} from "../build/wasm/stuff_worker.js";

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

function bucket(env: WorkerEnv, binding: string): R2BucketLike {
  const found = env[binding];
  if (!found || typeof found === "string" || typeof found.get !== "function") {
    throw new Error(`missing R2 binding: ${binding}`);
  }
  return found;
}

export default {
  async fetch(req: Request, env: WorkerEnv): Promise<Response> {
    if (req.method !== "GET" && req.method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }
    const mapped = JSON.parse(
      map_asset_json(env.STUFF_ROUTES, new URL(req.url).pathname),
    ) as Mapped;
    if (mapped.kind === "error") {
      throw new Error(mapped.message ?? "routes");
    }
    if (mapped.kind === "not_found" || !mapped.bucket_binding) {
      return new Response("not found", { status: 404 });
    }
    let key: string;
    let route: "immutable" | "pointer";
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
    }
    const store = bucket(env, mapped.bucket_binding);
    const object = await store.get(key);
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
