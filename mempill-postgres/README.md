# mempill-postgres

PostgreSQL persistence adapter for mempill (topology-b).

See the [root README](../README.md) for the full architecture, quick start, and invariants.

## What this crate provides

- `PostgresPersistenceStore` — `impl PersistencePort` backed by an r2d2 connection pool.
- `PostgresEngine<O, V>` — type alias for `EngineHandle<PostgresPersistenceStore, O, V>`.
- `open_postgres(conn_str, oracle, vector, config)` — open an engine connected to PostgreSQL.

## Usage

```rust
use mempill_postgres::{open_postgres, PostgresEngine};
use mempill_core::{EngineConfig, NoOpOracle, NoOpVector};

let engine: PostgresEngine<NoOpOracle, NoOpVector> = open_postgres(
    "host=localhost port=5432 user=mempill dbname=mempill password=secret",
    None,
    None,
    EngineConfig::default(),
)?;
```

## Concurrency model

- r2d2 connection pool (max 20 connections) — concurrent cross-agent transactions.
- Same-agent write serialization: `pg_advisory_xact_lock(hashtext(agent_id)::bigint)`.
- OCC belt-and-suspenders: `UNIQUE(agent_id, stream_seq)` on `ledger_entries`.
- `requires_global_write_serialization()` returns `false` — `EngineHandle` skips the
  global write Mutex, enabling true parallel writes across different agents.

## Schema migrations

Managed by refinery 0.9. The single `V1__*.sql` migration is embedded at compile time
via `refinery::embed_migrations!("migrations")`. Applied automatically on `open_postgres`.

## Current limitations

- **NoTls only** — suitable for local/Docker development. Production TLS is planned.
- Tested against PostgreSQL 16 (16.14) and PostgreSQL 18.4 via testcontainers.

## Integration tests

Require Docker. testcontainers-modules pulls `postgres:16` and `postgres:18.4` automatically.

```sh
cargo test -p mempill-postgres
```

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
