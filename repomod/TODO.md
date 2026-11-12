Prefix: RPMD
Next: 86

### RPMD-85 · P3 · Reconcile the remaining musl rollout and environment qualification

- **Area:** Operability
- **Where:** `musl-toolchain.toml:132`, `src/repomod/cmd/musl.py:91`, `docs/policy-development.md:282`
- **Problem:** The closed musl implementation work left owner-run canaries, shared-image consumer migration, and some environment qualification unresolved. Those external outcomes were not rechecked in this repository review, so completion must not be inferred from the presence of the lock and image recipe. This carries forward existing deferred work rather than asserting a new fleet defect. The draft below preserves the recovery pointers and owner deferrals from the removed campaign record.
- **Recommendation:** Recheck the named external outcomes before deciding what remains. Agree on the conventional-consumer migration scope, then follow the publication and canary gate in the policy guide. Keep owner-deferred Chromium/Node work and scheduling out of the run unless explicitly reopened. Delete this item after the remaining scope is completed or deliberately declined and its durable decisions are recorded.
- **Fix with:** `none`
- **Effort:** M (days)
- **Design:** [Musl rollout scope and qualification](docs/rfc/RPMD-85-musl-rollout.md)
