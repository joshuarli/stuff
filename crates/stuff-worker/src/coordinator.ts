interface ObjectHead { size: number; etag: string; text(): Promise<string> }
interface Bucket {
  get(key: string): Promise<ObjectHead | null>;
  head(key: string): Promise<ObjectHead | null>;
  put(key: string, value: string): Promise<unknown>;
  list(options: { prefix: string; cursor?: string }): Promise<{ objects: { key: string }[]; truncated: boolean; cursor?: string }>;
  delete(key: string): Promise<void>;
}
interface SQL { exec<T = Record<string, unknown>>(query: string, ...bindings: (string | number | null)[]): Iterable<T> }
interface State { storage: { sql: SQL; transactionSync<T>(fn: () => T): T; sync(): Promise<void> }; blockConcurrencyWhile<T>(fn: () => Promise<T>): Promise<T> }
interface Env { MYCRATES: Bucket; MYNPM: Bucket; PYPI: Bucket; OCI: Bucket; OCI_BLOBS: Bucket; MIRROR: Bucket }
type Current = { revision: number; generation: string | null; manifest: string | null; pending: number };
type Lease = { kind: string; token: string; expires: number };
type Receipt = { payload: string; revision: number; generation: string; manifest: string };
const bindings: Record<string, keyof Env> = { cargo: "MYCRATES", npm: "MYNPM", pypi: "PYPI", oci: "OCI", mirror: "MIRROR" };
const validGeneration = (value: unknown): value is string => typeof value === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(value);
const validToken = (value: unknown): value is string => typeof value === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(value);
const response = (status: number, value: unknown): Response => Response.json(value, { status, headers: { "cache-control": "no-store" } });
async function hash(value: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
export class PublicationCoordinator {
  private readonly state: State;
  private readonly env: Env;
  constructor(state: State, env: Env) {
    this.state = state;
    this.env = env;
    const sql = state.storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS current (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL, generation TEXT, manifest TEXT, pending INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS lease (id INTEGER PRIMARY KEY CHECK(id=1), kind TEXT NOT NULL, token TEXT NOT NULL, expires INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS receipts (key TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL, generation TEXT NOT NULL, manifest TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS retired_generations (generation TEXT PRIMARY KEY)");
  }
  private one<T>(query: string, ...args: (string | number | null)[]): T | undefined { return Array.from(this.state.storage.sql.exec<T>(query, ...args))[0]; }
  private async manifest(store: Bucket, generation: string): Promise<{ identity: string; objects: { key: string; size: number; etag: string }[] }> {
    const object = await store.get(`manifests/${generation}.json`);
    if (!object) throw new Error("manifest missing");
    const text = await object.text();
    const value: unknown = JSON.parse(text);
    if (!value || typeof value !== "object" || !("schemaVersion" in value) || value.schemaVersion !== 1 || !("objects" in value) || !Array.isArray(value.objects)) throw new Error("invalid manifest");
    const objects: { key: string; size: number; etag: string }[] = [];
    for (const item of value.objects as unknown[]) {
      if (!item || typeof item !== "object" || !("key" in item) || typeof item.key !== "string" || (item.key.startsWith("/") || Array.from(item.key).some((c) => c.charCodeAt(0) < 32)) || item.key.split("/").some((part) => !part || part === "." || part === "..") || !("size" in item) || !Number.isSafeInteger(item.size) || (item.size as number) < 0 || !("etag" in item) || typeof item.etag !== "string" || !item.etag) throw new Error("invalid manifest object");
      objects.push({ key: item.key, size: item.size as number, etag: item.etag });
    }
    return { identity: await hash(text), objects };
  }
  private async current(store: Bucket): Promise<Current> {
    let row = this.one<Current>("SELECT revision, generation, manifest, pending FROM current WHERE id=1");
    if (!row) {
      const marker = await store.get("current");
      const generation = marker ? (await marker.text()).trim() : null;
      if (generation !== null && !validGeneration(generation)) throw new Error("invalid current");
      const manifest = generation === null ? null : (await this.manifest(store, generation)).identity;
      this.state.storage.sql.exec("INSERT INTO current VALUES (1, 0, ?, ?, 0)", generation, manifest);
      row = { revision: 0, generation, manifest, pending: 0 };
    }
    const projected = await store.get("current");
    const projectedGeneration = projected ? (await projected.text()).trim() : null;
    if (row.pending || projectedGeneration !== row.generation) {
      if (row.generation === null) throw new Error("invalid projection");
      if (projectedGeneration !== row.generation) await store.put("current", row.generation);
      this.state.storage.sql.exec("UPDATE current SET pending=0 WHERE id=1 AND revision=?", row.revision);
      row.pending = 0;
    }
    return row;
  }
  private lease(kind: string, token: unknown): boolean {
    const lease = this.one<Lease>("SELECT kind, token, expires FROM lease WHERE id=1");
    return validToken(token) && lease?.kind === kind && lease.token === token && lease.expires > Date.now();
  }
  async fetch(req: Request): Promise<Response> {
    return this.state.blockConcurrencyWhile(async () => {
      try { return await this.handle(req); } catch { return response(503, { error: "coordinator operation failed" }); }
    });
  }
  private async handle(req: Request): Promise<Response> {
    const [, registry, op] = new URL(req.url).pathname.split("/");
    const binding = registry ? bindings[registry] : undefined;
    if (!binding || !op) return response(404, { error: "not found" });
    const store = this.env[binding];
    const current = await this.current(store);
    if (op === "current" && req.method === "GET") return response(200, current);
    if (req.method !== "POST") return response(405, { error: "method not allowed" });
    const length = Number(req.headers.get("content-length") ?? "0");
    if (!Number.isFinite(length) || length > 16384) return response(413, { error: "too large" });
    const text = await req.text();
    if (text.length > 16384) return response(413, { error: "too large" });
    const body = JSON.parse(text) as Record<string, unknown>;
    if (!body || typeof body !== "object" || Array.isArray(body)) return response(400, { error: "bad request" });
    const sql = this.state.storage.sql;
    if (op === "begin" || op === "gc-claim") {
      const existing = this.one<Lease>("SELECT kind, token, expires FROM lease WHERE id=1");
      if (existing && existing.expires > Date.now()) return response(409, { error: "lease held" });
      const token = crypto.randomUUID();
      const expires = Date.now() + 300000;
      sql.exec("INSERT OR REPLACE INTO lease VALUES (1, ?, ?, ?)", op === "begin" ? "upload" : "gc", token, expires);
      return response(200, { token, expires, revision: current.revision });
    }
    if (op === "renew" || op === "abort") {
      if (!this.lease("upload", body.token) && !this.lease("gc", body.token)) return response(409, { error: "stale lease" });
      if (op === "abort") sql.exec("DELETE FROM lease WHERE id=1");
      else sql.exec("UPDATE lease SET expires=? WHERE id=1", Date.now() + 300000);
      return response(200, { ok: true });
    }
    if (op === "activate" || op === "rollback") {
      if (!validToken(body.key) || !validGeneration(body.generation) || typeof body.manifest !== "string" || !/^[0-9a-f]{64}$/.test(body.manifest) || !Number.isSafeInteger(body.expectedRevision)) return response(400, { error: "bad request" });
      const payload = JSON.stringify([op, body.generation, body.manifest, body.expectedRevision]);
      const receipt = this.one<Receipt>("SELECT payload, revision, generation, manifest FROM receipts WHERE key=?", body.key);
      if (receipt) return receipt.payload === payload ? response(200, { revision: receipt.revision, generation: receipt.generation, manifest: receipt.manifest }) : response(409, { error: "idempotency conflict" });
      if (!this.lease("upload", body.token)) return response(409, { error: "stale lease" });
      if (current.revision !== body.expectedRevision) return response(409, { error: "revision conflict" });
      const manifest = await this.manifest(store, body.generation);
      if (manifest.identity !== body.manifest) return response(409, { error: "manifest conflict" });
      const retired = new Set(Array.from(sql.exec<{ generation: string }>("SELECT generation FROM retired_generations"), row => row.generation));
      if (retired.has(body.generation) || manifest.objects.some(item =>
        (item.key.startsWith("generations/") && retired.has(item.key.split("/")[1]!)) ||
        (item.key.startsWith("manifests/") && retired.has(item.key.slice("manifests/".length).replace(/\.json$/, "")))))
        return response(409, { error: "generation permanently retired by garbage collection" });
      for (const item of manifest.objects) {
        const found = await store.head(item.key);
        if (!found || found.size !== item.size || found.etag.replaceAll('"', "") !== item.etag.replaceAll('"', "")) return response(409, { error: "incomplete generation" });
      }
      if (registry === "oci") {
        const object = await store.get(`generations/${body.generation}/blobs.json`);
        if (!object) return response(409, { error: "OCI blob inventory missing" });
        const inventory = JSON.parse(await object.text()) as { schemaVersion?: unknown; blobs?: unknown };
        if (inventory.schemaVersion !== 1 || !Array.isArray(inventory.blobs)) return response(409, { error: "invalid OCI blob inventory" });
        for (const blob of inventory.blobs as unknown[]) {
          if (typeof blob !== "string" || !/^sha256:[0-9a-f]{64}$/.test(blob) || !await this.env.OCI_BLOBS.head(`sha256/${blob.slice(7)}`))
            return response(409, { error: "OCI blob missing or invalid" });
        }
      }
      if (!this.lease("upload", body.token)) return response(409, { error: "stale lease" });
      const revision = current.revision + 1;
      this.state.storage.transactionSync(() => {
        sql.exec("UPDATE current SET revision=?, generation=?, manifest=?, pending=1 WHERE id=1", revision, body.generation as string, body.manifest as string);
        sql.exec("INSERT INTO receipts VALUES (?, ?, ?, ?, ?)", body.key as string, payload, revision, body.generation as string, body.manifest as string);
        sql.exec("DELETE FROM lease WHERE id=1");
      });
      await this.state.storage.sync();
      await this.current(store);
      return response(200, { revision, generation: body.generation, manifest: body.manifest });
    }
    if (op === "gc-delete") {
      if (!this.lease("gc", body.token)) return response(409, { error: "stale lease" });
      if (!validGeneration(body.generation) || body.generation === current.generation) return response(409, { error: "invalid generation" });
      const prefix = `generations/${body.generation}/`;
      let cursor: string | undefined;
      const keys: string[] = [];
      do {
        const listed = await store.list({ prefix, ...(cursor ? { cursor } : {}) });
        keys.push(...listed.objects.map((object) => object.key));
        cursor = listed.truncated ? listed.cursor : undefined;
        if (listed.truncated && !cursor) throw new Error("missing cursor");
      } while (cursor);
      const deleting = new Set(keys);
      cursor = undefined;
      do {
        const listed = await store.list({ prefix: "manifests/", ...(cursor ? { cursor } : {}) });
        for (const object of listed.objects) {
          const match = /^manifests\/([a-zA-Z0-9_-]{1,128})\.json$/.exec(object.key);
          if (!match) throw new Error("invalid manifest key");
          if (match[1] !== body.generation) {
            const retained = await this.manifest(store, match[1]!);
            if (retained.objects.some((item) => deleting.has(item.key))) return response(409, { error: "referenced generation" });
          }
        }
        cursor = listed.truncated ? listed.cursor : undefined;
        if (listed.truncated && !cursor) throw new Error("missing cursor");
      } while (cursor);
      keys.push(`manifests/${body.generation}.json`);
      if (!this.lease("gc", body.token)) return response(409, { error: "stale lease" });
      sql.exec("INSERT INTO retired_generations VALUES (?) ON CONFLICT(generation) DO NOTHING", body.generation);
      await this.state.storage.sync();
      for (const key of keys) {
        if (!this.lease("gc", body.token)) return response(409, { error: "stale lease" });
        await store.delete(key);
      }
      return response(200, { deleted: keys.length });
    }
    return response(404, { error: "not found" });
  }
}
