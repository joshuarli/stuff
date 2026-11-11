# STUF-63: Evaluate the Worker WASM interface

Status: draft

## Summary

Compare an explicit contract on the current wasm-bindgen ABI with a small WIT/component-model prototype. Keep the existing build and deployment path until an alternative proves useful in this repository.

## Motivation

`crates/stuff-worker/src/wasm.rs` exposes synchronous operations that `crates/stuff-worker/entry.ts` calls using strings and primitives. The build script now synchronizes the bindgen executable with `Cargo.lock`, so the earlier CLI version skew is resolved. The R2 nullability, pagination, and body-handling defects are host-adapter problems; a WASM migration does not fix them.

## Design

Limit the experiment to three representative boundaries: route mapping, an object response plan, and aggregate catalog rendering. Keep the externally visible routes and response bytes unchanged. Compare the current ABI with explicit/generated types against WIT using JSON strings first; assess typed records only where they remove concrete validation or duplication.

Require `crates/stuff-worker/scripts/build.mjs` output to bundle, its workerd harness to execute the exports, and the existing `cf` deployment format to accept the result. Record generated code, artifact size, error propagation, initialization, and a measured request-path cost. Stop the experiment if the toolchain cannot support that path without a second permanent host implementation.

## Alternatives considered

- **Retain bindgen and make boundary types explicit:** smallest change and the recommended baseline.
- **WIT with JSON strings:** an explicit operation contract with limited data-model migration.
- **WIT with typed records:** a larger contract change that needs evidence beyond replacing serialization syntax.

## Open questions

- What demonstrated problem would justify migration after bindgen version synchronization? Recommendation: require a measurable maintenance or correctness benefit.
- Should a successful prototype use JSON payloads or typed records? Recommendation: preserve JSON initially and change only contracts whose validation burden demonstrably improves.

## Decision

Pending a bounded experiment. Correct the existing host-boundary findings independently. Delete this RFC when the experiment is rejected or its selected design is implemented, then document the live interface in `crates/stuff-worker/README.md`.
