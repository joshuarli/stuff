# STUF-62: Share unchanged index bytes across generations

Status: draft

## Summary

Evaluate whether reducing generation-scoped index uploads is worth an additional metadata lookup. Preserve immutable generations, static native serving, retained-generation rollback, and existing published formats.

## Motivation

`crates/stuff/src/publish/r2/sync.rs` compares full object keys. A new generation assigns unchanged index files new keys, so publication work grows with the entire registry. This is established from source; earlier timing figures have not been remeasured. The current representation has the useful property that a generation path directly names its bytes.

## Design

First measure a one-crate addition against a representative retained registry: uploaded object count and bytes, R2 operations, cold/warm publication time, and native/Worker request cost. Check successful output and run repeated interleaved comparisons before reporting a speedup.

Prototype a generation-local path-to-digest manifest that references shared immutable index objects. A generation continues to describe a complete snapshot; readers never combine it with mutable latest-state data. Native publication may materialize files or hardlinks for static serving. The Worker resolves only within the pinned generation and caches immutable mappings.

GC must traverse every retained manifest before reclaiming shared index bytes. Promotion verifies the full referenced set; rollback remains valid while its generation is retained. New readers must keep reading existing generation-prefixed objects, and format activation follows reader deployment. Prototype this without changing current published formats.

## Alternatives considered

- **Keep direct generation keys:** simplest reads and recovery; retains write amplification.
- **Share index bytes through immutable mappings:** fewer uploads, but adds a reader contract and GC reachability rules.
- **Change HTTP connection reuse only:** may reduce request overhead, but cannot reduce the number of generation keys. Any h12tiny change belongs upstream because its imported sources must stay byte-identical.

## Open questions

- Is measured publication cost large enough to justify a new storage format? Recommendation: keep direct keys until the prototype demonstrates a useful end-to-end improvement.
- Can the extra Worker lookup meet the read-cost and latency budget? Recommendation: evaluate this with the same realistic catalogs used for publication measurements.

## Decision

Pending measurement. Do not migrate storage as part of the review fixes. Delete the RFC after rejection or implementation; preserve the chosen format and GC invariants in the owning durable docs.
