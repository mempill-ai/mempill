# mempill-types

Domain types for the mempill AI-agent memory engine.

This crate is a dependency of all other mempill crates. It has no runtime dependency on
Tokio or any persistence layer — only `chrono`, `serde`, `serde_json`, and `uuid`.

## Contents

| Module | Key types |
|---|---|
| `provenance` | `ProvenanceLabel` (3-channel enum), `ExternalKind`, `ExternalAnchor` |
| `disposition` | `Disposition` (12-state enum), `WriteOutcome` |
| `claim` | `Claim`, `Cardinality`, `Confidence`, `Criticality` |
| `validity` | `ValidTime`, `ValidityAssertion` |
| `identity` | `ClaimRef` (UUID newtype), `AgentId` (String newtype) |
| `belief` | `BeliefProjection`, `BeliefStatus`, `ProjectedFact` |
| `ledger` | `LedgerEntry` |
| `edge` | `ClaimEdge`, `EdgeKind` |
| `proposal` | `Proposal` (returned by oracle/extractor ports; never commits directly) |
| `time` | Time helpers |

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
