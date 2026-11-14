// Throwaway rehearsal app for the shared OpenOrange stack: one SQLite-backed
// Durable Object plus an endpoint that reports the gateway headers it sees.
// OO_GATEWAY_SECRET is the per-app secret Caddy forwards; only a boolean match is
// ever returned.
import { DurableObject } from "cloudflare:workers";

interface Env {
  COUNTER: DurableObjectNamespace<Counter>;
  OO_GATEWAY_SECRET?: string;
  DATA: R2Bucket;
}

export class Counter extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS hits (n INTEGER NOT NULL)");
    ctx.storage.sql.exec("INSERT INTO hits (n) SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM hits)");
  }
  hit(): number {
    this.ctx.storage.sql.exec("UPDATE hits SET n = n + 1");
    return this.get();
  }
  get(): number {
    return this.ctx.storage.sql.exec<{ n: number }>("SELECT n FROM hits").one().n;
  }
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const h = request.headers;
    if (url.pathname === "/whoami") {
      const secret = h.get("stats-gateway-secret");
      return Response.json({
        gatewaySecretMatches: Boolean(env.OO_GATEWAY_SECRET) && secret === env.OO_GATEWAY_SECRET,
        tailnetIdentity: h.get("stats-tailnet-identity"),
        tailscaleUserLogin: h.get("tailscale-user-login"),
        xForwardedFor: h.get("x-forwarded-for"),
        host: h.get("host"),
        ingress: h.get("x-oo-ingress"),
      });
    }
    if (url.pathname === "/r2") {
      // Object-store round trip through the per-app S3 key and bucket.
      const key = "demo/" + (url.searchParams.get("k") ?? "x");
      if (request.method === "PUT") {
        const r = await env.DATA.put(key, await request.text(), { onlyIf: url.searchParams.has("create") ? { etagDoesNotMatch: "*" } : undefined });
        return Response.json({ stored: r !== null, etag: r?.etag ?? null });
      }
      const obj = await env.DATA.get(key);
      return obj ? Response.json({ value: await obj.text(), etag: obj.etag }) : new Response("missing", { status: 404 });
    }
    if (url.pathname === "/admin" || url.pathname.startsWith("/admin/")) {
      return new Response("admin reached\n");
    }
    const stub = env.COUNTER.getByName("demo");
    if (url.pathname === "/hit") return Response.json({ n: await stub.hit() });
    if (url.pathname === "/count") return Response.json({ n: await stub.get() });
    return new Response("oo-demo\n");
  },
};
