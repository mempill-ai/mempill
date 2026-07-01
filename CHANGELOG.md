# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.0]

### Added

- **Valid-time point-in-time query (`valid_at`)** — recall the belief as it was valid at a
  specific instant (e.g. "who was CEO in 2021?"), independent of `as_of_tx_time`. `valid_at`
  and `as_of_tx_time` are separate query axes: one asks what was *true* at a point in time,
  the other asks what the engine *believed* at a point in time. Available in Rust, Python,
  and MCP.
- **Date granularity for valid-time bounds** — `ValidTime.start` and `ValidTime.end` each
  carry an independent `DateGranularity` (`Year` / `Month` / `Day` / `Instant`). Display is
  honest about precision: a `Month` bound renders as `"2020-03"`, a `Year` bound as `"2020"`,
  a `Day` bound as `"2020-03-15"` — no fabricated precision beyond what was supplied. The
  ergonomic `remember()` API infers granularity automatically from the shape of the supplied
  date string; structured ingest paths (`IngestClaimRequest`, the Python dict API, and MCP)
  require the caller to specify granularity explicitly. Legacy rows written before this
  feature have `None` granularity and continue to display as `YYYY-MM-DD`. Covered by the
  cross-adapter conformance suite (SQLite and PostgreSQL).
- **`query_subject`** — subject-scoped enumeration of all resolved beliefs for a given
  subject, across every predicate recorded for it. Bi-temporal aware: respects the same
  `valid_at` / `as_of_tx_time` semantics as single-predicate queries.

### Fixed

- **Transaction-time correctness** — `as_of_tx_time` now correctly scopes both disposition
  lookup and subject-line loading to the requested transaction-time window. Previously, a
  query with `as_of_tx_time` set could read disposition state or subject-line data outside
  the requested window, producing results inconsistent with the transaction-time snapshot
  the caller asked for.
- **PostgreSQL `tx_time` column binding** — corrected binding of the `tx_time` column, which
  is stored as `TEXT` in the PostgreSQL schema, so transaction-time comparisons and ordering
  behave identically to the SQLite adapter.
