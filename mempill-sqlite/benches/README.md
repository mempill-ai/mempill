# mempill-sqlite — bi-temporal as-of benchmark

## What this benchmark measures

mempill's core differentiator is **true bi-temporal querying**: two independent time
axes that can be queried simultaneously.

| Axis | Request field | Semantics |
|---|---|---|
| valid-time | `valid_at` | "Who was CEO on 2021-06-01?" |
| transaction-time | `as_of_tx_time` | "What did we believe as of 2023-03-01?" |

No mainstream agent-memory library benchmarks these axes independently.
Publishing this micro-benchmark is itself the contribution: it lets adopters
measure precisely where latency comes from and proves that mempill's read-time
fold cost scales linearly (not exponentially) with history depth N.

### Benchmark scenarios

All scenarios build a succession corpus of N non-overlapping claims on a single
`(agent, subject, predicate)` line using the real SQLite adapter (end-to-end, no mocks).
The corpus is built OUTSIDE the measured closure; only the query call is timed.
All timestamps are deterministic fixed constants — `Utc::now()` is never called
inside a measured loop.

| ID | Group name | `valid_at` | `as_of_tx_time` | What it measures |
|---|---|---|---|---|
| A | `bench_a_valid_at` | set | unset | Independent valid-time axis selection |
| B | `bench_b_as_of_tx_time` | unset | set | Transaction-time filtering cost |
| C | `bench_c_combined_valid_at_and_as_of` | set | set | Full D2-independence (both axes) |
| D | `bench_d_baseline_current_belief` | unset | unset | No time-travel baseline |

### The D2-independence case (Bench C)

Bench C is the novel scenario: both axes set independently. The tx-time filter
runs first (eliminates claims ingested after `as_of_tx_time`), then the valid-time
fold selects the single claim whose window contains `valid_at`. No competitor
library offers or benchmarks this path.

## How to run

```sh
# Run all four groups (default: 100-sample Criterion measurement per input size):
cargo bench -p mempill-sqlite

# Run a single group:
cargo bench -p mempill-sqlite -- bench_a_valid_at

# HTML reports (Criterion produces them automatically):
open target/criterion/report/index.html
```

## Results table

Measured on a dev laptop (Apple M-series, macOS, `--release` profile).
**Indicative figures — not certified performance numbers.**

The query path is: `QueryMemoryUseCase::execute_with_time` →
`SqlitePersistenceStore::load_subject_line` (+ `load_ledger_for_claims`) → `truth_engine::fold`.
Each row is the mean time-per-iteration at 100 Criterion samples.

### Bench A — `valid_at` point query (ns per iteration)

| N (history depth) | mean µs |
|---|---|
| 1 | ~20 µs |
| 5 | ~46 µs |
| 20 | ~137 µs |
| 50 | ~321 µs |

### Bench B — `as_of_tx_time` query

| N | mean µs |
|---|---|
| 1 | ~20 µs |
| 5 | ~46 µs |
| 20 | ~136 µs |
| 50 | ~327 µs |

### Bench C — combined `valid_at` + `as_of_tx_time` (D2-independence)

| N | mean µs |
|---|---|
| 1 | ~20 µs |
| 5 | ~46 µs |
| 20 | ~138 µs |
| 50 | ~323 µs |

### Bench D — baseline (current belief, no time-travel)

| N | mean µs |
|---|---|
| 1 | ~20 µs |
| 5 | ~46 µs |
| 20 | ~136 µs |
| 50 | ~321 µs |

## Scaling analysis (the honest story)

Read cost grows **linearly with history depth N**:

- N=1 → ~20 µs
- N=5 → ~46 µs (2.3x)
- N=20 → ~137 µs (6.8x)
- N=50 → ~323 µs (16x)

The dominant cost is SQLite row retrieval (two queries per call: `load_subject_line`
+ `load_ledger_for_claims`). The in-memory fold (`truth_engine::fold`) is negligible
relative to SQLite I/O at all tested N values.

**Key finding:** all four query modes (A, B, C, D) show nearly identical cost at
each N. Adding the bi-temporal time-travel axes (valid_at, as_of_tx_time) does not
add measurable overhead — the extra work is in the SQL `WHERE recorded_at <= ?`
filter, not in the fold. mempill's bi-temporal query is not more expensive than a
plain "current belief" query at the same history depth.

**Implication for adopters:** history depth — not query type — is the primary cost driver.
For N ≤ 20 (the typical regime for agent working memory), queries complete in ~137 µs
or less against an in-memory SQLite store, well within real-time agent loop budgets.
At N=50, ~323 µs is still acceptable for non-critical paths; for latency-sensitive
use cases with deep histories, consider pruning superseded claims or indexing by agent/subject.
