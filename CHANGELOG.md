# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Version headings are dated at publish time. A version with no date and the
`— Unreleased` suffix is on `main` but has not yet been published to crates.io/PyPI.

## [0.4.0] — Unreleased

### Changed (Breaking)

- **`mempill-sqlite` public entry points replaced with per-agent constructors.**
  `open_default(path)` and `open_with_oracle(path, oracle)` are no longer public; the
  raw path-based `connection::open` is now `pub(crate)` only. Use instead:
  - `open_default_for_agent(base_dir, agent_id)` (was `open_default(path)`)
  - `open_with_oracle_for_agent(base_dir, agent_id, oracle)` (was `open_with_oracle(path, oracle)`)

  The database file is now derived automatically as `base_dir/agent_{agent_id}.db`,
  making it structurally impossible for two different `agent_id`s to collide on the
  same file. `agent_id` is validated against `[A-Za-z0-9_-]`
  (`SqliteStoreError::InvalidAgentId` on violation). `open_default_in_memory()` /
  `open_with_oracle_in_memory()` are unchanged.
  - `mempill` facade: `open_default_for_agent(base_dir, agent_id)` replaces
    `open_default(path)`.
  - `mempill-python`: `mempill.open_for_agent(base_dir, agent_id)` replaces
    `mempill.open(path)`; `mempill.open_oracle_for_agent(base_dir, agent_id, oracle)`
    replaces `mempill.open_oracle(path, oracle)`.
  - `mempill-mcp`: the `MEMPILL_DB_PATH` (full file path) environment variable is
    replaced by `MEMPILL_DB_DIR` (base directory) — the server now calls
    `mempill.open_for_agent(base_dir, agent_id)` internally.
  - **Migration:** pre-0.4.0 shared-file databases are not auto-migrated. Point
    `MEMPILL_DB_DIR` / `base_dir` at a fresh directory, or manually copy an existing
    database file to `base_dir/agent_{agent_id}.db` before first use.
  - PostgreSQL entry points (`open_postgres`, `open_postgres_with_oracle`) are
    **unaffected** — PostgreSQL was already agent-scoped via advisory locking, not
    file naming.

### Added

- **Configurable PostgreSQL connection pool.** New `PoolConfig` struct
  (`max_size: u32`, `connection_timeout: Duration`) and an additive
  `PostgresPersistenceStore::with_pool_config(conn_str, pool_config)` constructor.
  `PostgresPersistenceStore::new` is unchanged and delegates to
  `with_pool_config` with `PoolConfig::default()` (`max_size = 20`,
  `connection_timeout = 5s`) — existing callers are unaffected. `max_size == 0` is
  rejected with `PostgresStoreError::Config` before any network connection is
  attempted.
- **As-of / bi-temporal correctness benchmark.** New reproducible, assertion-based
  benchmark at `mempill-facade/examples/asof_correctness_benchmark.rs`, covering
  both the valid-time and transaction-time axes independently (including the
  3-way succession chain and the honest Contested case). Run it with:

  ```sh
  cargo run --release --example asof_correctness_benchmark -p mempill
  ```

  This is a correctness benchmark only — no timing/latency numbers are captured
  or published. See `tasks/25-storage-and-roadmap-merged/AS_OF_CORRECTNESS_BENCHMARK.md`
  for the full results table and results published on the
  [documentation site](https://mempill.netlify.app/concepts/benchmark-results/).
- **Date granularity on `query_history` / `history()`.** `HistoryEntry` gained two
  additive fields, `valid_from_granularity` and `valid_until_granularity`
  (`Option<DateGranularity>`), closing the one honest-display read path that
  previously dropped stored precision (`query_memory` / `query_subject` already had
  it). `valid_until_granularity` is a **derived** value: because `valid_until` is
  bounded by the successor claim's canonical ordering key (supersession), not stored
  on the entry itself, the honest granularity to report is the **successor's**
  `start_granularity` — never this entry's own (never-populated) `end_granularity`.
  When the successor's ordering key falls back to `transaction_time` (low valid-time
  confidence), `valid_until_granularity` is `None`. The Rust facade's `history()`
  passes both fields through verbatim (`HistoryEntry` is re-exported directly from
  `mempill-core`, no facade change needed). The Python wheel's `query_history` is now
  enriched the same way `query_memory` is (`enrich_query_history` mirrors
  `enrich_query_memory`): every entry gains pre-rendered `valid_from_display` /
  `valid_until_display` strings, plus the raw granularity fields; the ergonomic
  `HistoryEntry` dataclass and `.pyi` stubs were updated (stubtest stays clean). New
  `run_history_granularity_conformance` harness proves parity across SQLite and
  Postgres (16 and 18), including a three-way Month→Day→Year succession asserting the
  derived-endpoint rule. Non-breaking: additive fields only.

### Fixed

- **Ledger scope on write/audit paths.** Replaced a capped, agent-wide
  `load_ledger(agent_id, None, 10_000)` call with an uncapped, claim-scoped
  `load_ledger_for_claims` lookup on all write and audit paths (`ingest_claim`,
  `reconcile`, `submit_adjudication`, `sweep_adjudications`). Previously,
  disposition-changing ledger entries beyond the 10,000-row cap window were
  silently invisible to fold/state-guard logic on agents with a large ledger
  history, which could produce an incorrect belief. Correctness is now
  independent of agent ledger history size.
- The `mempill` facade now re-exports `PoolConfig` from `mempill::postgres`, so the
  configurable connection pool is usable without a direct `mempill-postgres`
  dependency.
- SQLite schema migrations now run inside a single immediate transaction;
  concurrent first opens of a brand-new per-agent database file no longer fail
  with `duplicate column name`.

### Notes

- A valid-time point claim (`start == end`) is accepted, but yields `NoBelief` at
  that instant: valid-time windows are half-open `[start, end)`, so a
  zero-length window never contains its own boundary.

## [0.3.0] — 2026-07-02

### Changed

- **Reconciler succession classification for non-overlapping valid-time windows.** When two
  claims with confident (>= 0.7) valid-time confidence assert different values on the same
  subject/predicate with non-overlapping valid-time windows, they are now classified as a
  succession: the incoming claim receives `Disposition::CommittedCheap`, and point-in-time
  queries return `Resolved` — the claim valid at the query instant — instead of `Contested`.
  This produces correct answers for historical state queries (e.g. "who was CEO in 2021?").
  Overlapping valid-time windows, missing `valid_time`, or confidence below 0.7 continue to
  yield `Contested` as before.

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
