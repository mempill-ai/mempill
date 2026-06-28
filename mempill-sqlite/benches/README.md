> Internal micro-benchmark — not a published performance benchmark; see Notes.

# mempill-sqlite — bi-temporal as-of benchmark

## What this measures

mempill supports two independent time axes for querying memory:

| Axis | Request field | Semantics |
|---|---|---|
| valid-time | `valid_at` | "Who was CEO on 2021-06-01?" |
| transaction-time | `as_of_tx_time` | "What did we believe as of 2023-03-01?" |

This micro-benchmark exercises the query path for each axis in isolation and in
combination, to observe how read cost scales with history depth N.

### Benchmark scenarios

All scenarios build a succession corpus of N non-overlapping claims on a single
`(agent, subject, predicate)` line using the real SQLite adapter (end-to-end, no mocks).
The corpus is built OUTSIDE the measured closure; only the query call is timed.
All timestamps are deterministic fixed constants — `Utc::now()` is never called
inside a measured loop.

| ID | Group name | `valid_at` | `as_of_tx_time` | What it measures |
|---|---|---|---|---|
| A | `bench_a_valid_at` | set | unset | Valid-time axis selection |
| B | `bench_b_as_of_tx_time` | unset | set | Transaction-time filtering cost |
| C | `bench_c_combined_valid_at_and_as_of` | set | set | Both axes set independently |
| D | `bench_d_baseline_current_belief` | unset | unset | No time-travel baseline |

## How to run

```sh
# Run all four groups (default: 100-sample Criterion measurement per input size):
cargo bench -p mempill-sqlite

# Run a single group:
cargo bench -p mempill-sqlite -- bench_a_valid_at

# HTML reports (Criterion produces them automatically):
open target/criterion/report/index.html
```

## Notes

This is a **local micro-benchmark** intended for two internal purposes:

1. **Regression detection** — catch query-path overhead regressions between commits.
2. **History-depth characterization** — observe how read cost scales as N (number of
   claims on a single subject line) grows.

Results are environment-dependent (hardware, OS, SQLite page cache state). The figures
you observe on your machine are not comparable to results from another environment.

**These numbers are NOT a published or certified performance benchmark and must not be
cited as such.**

Qualitative engineering finding: read cost grows with history depth N (driven primarily
by SQLite row retrieval — two queries per call: `load_subject_line` +
`load_ledger_for_claims`). The in-memory fold (`truth_engine::fold`) is negligible
relative to SQLite I/O. All four query modes (A, B, C, D) show similar cost at each N:
enabling the bi-temporal time-travel axes does not add measurable overhead versus a
plain current-belief query at the same history depth.

Run the benchmark on your own hardware to get figures relevant to your deployment context.
