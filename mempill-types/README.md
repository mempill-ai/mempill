# mempill-types

Domain types for the mempill AI-agent memory engine.

This crate is a dependency of all other mempill crates. It has no runtime dependency on
Tokio or any persistence layer — only `chrono`, `serde`, `serde_json`, and `uuid`.

## Contents

| Module | Key types |
|---|---|
| `provenance` | `ProvenanceLabel` (3-channel enum), `ExternalKind`, `ExternalAnchor` |
| `disposition` | `Disposition` (12-state enum), `WriteOutcome` |
| `claim` | `Claim`, `Fact`, `Cardinality`, `Confidence`, `Criticality` |
| `validity` | `ValidityAssertion`, `AssertionKind` |
| `identity` | `ClaimRef` (UUID newtype), `AgentId` (String newtype), `SubjectLineRef` |
| `belief` | `BeliefProjection`, `BeliefStatus`, `Belief` |
| `ledger` | `LedgerEntry`, `LedgerEventKind` |
| `edge` | `ClaimEdge`, `EdgeKind` |
| `proposal` | `ClaimProposal`, `AdjudicationRequest`/`Response`/`Outcome` (returned by oracle/extractor ports; never commits directly) |
| `time` | `ValidTime`, `DateGranularity`, `TransactionTime` |

## License

Apache-2.0. See [LICENSE](LICENSE) for the full text.
