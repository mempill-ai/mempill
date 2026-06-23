# mempill-ts

**Status: planned — not yet implemented.**

This crate is a placeholder for TypeScript / Node.js FFI bindings to `mempill-core` via
napi-rs. The crate compiles (it depends on `mempill-core`) but contains no binding logic.

Binding implementation is planned for a future release. When shipped it will provide a Node.js
native module exposing the same four operations as the Python wheel: `ingest_claim`,
`query_memory`, `reconcile`, and `query_audit`.

See the [root README](../README.md) for the full architecture and roadmap.

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
