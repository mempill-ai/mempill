-- migrations/V1__initial_schema.sql
-- mempill-postgres: PostgreSQL DDL porting SQLite schema.
-- Behavioral contract identical; storage divergences documented inline.
-- PG16 target. INSERT-only design (no UPDATE/DELETE of claims).

-- ── CLAIMS ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS claims (
    id                          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    claim_id                    TEXT        NOT NULL UNIQUE,   -- UUID as TEXT (wire-compat with SQLite)
    agent_id                    TEXT        NOT NULL,
    subject                     TEXT        NOT NULL,
    predicate                   TEXT        NOT NULL,
    value                       JSONB       NOT NULL,          -- DIVERGENCE: JSONB (not TEXT)
    cardinality                 TEXT        NOT NULL DEFAULT 'Unknown',
    provenance_label            TEXT        NOT NULL,
    nearest_external_anchor_id  TEXT,
    derivation_depth            INTEGER     NOT NULL DEFAULT 0,
    tx_time                     TEXT        NOT NULL,          -- ISO-8601 UTC as TEXT (wire-compat)
    valid_time_start            TEXT,
    valid_time_end              TEXT,
    valid_time_confidence       DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    value_confidence            DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    criticality                 TEXT        NOT NULL DEFAULT 'Medium',
    derived_from                TEXT        NOT NULL DEFAULT '[]',  -- JSON array as TEXT
    metadata                    JSONB,                         -- DIVERGENCE: JSONB (not TEXT)
    snapshot_schema_version     INTEGER,
    embedding_model_id          TEXT
    -- No UNIQUE(agent_id, subject, predicate, value): idempotency enforced in Rust (I6)
);

-- ── VALIDITY ASSERTIONS ───────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS validity_assertions (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    assertion_id          TEXT        NOT NULL UNIQUE,
    agent_id              TEXT        NOT NULL,
    target_claim_id       TEXT        NOT NULL REFERENCES claims(claim_id),
    assertion_kind        TEXT        NOT NULL,
    bound_at              TEXT,
    reopen_at             TEXT,
    provenance_label      TEXT        NOT NULL,
    value_confidence      DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    valid_time_confidence DOUBLE PRECISION NOT NULL DEFAULT 0.5,
    asserted_at           TEXT        NOT NULL
);

-- ── LEDGER ENTRIES ────────────────────────────────────────────────────────────
-- PG-ONLY: stream_seq for OCC belt-and-suspenders (A41).
CREATE TABLE IF NOT EXISTS ledger_entries (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    entry_id     TEXT        NOT NULL UNIQUE,
    agent_id     TEXT        NOT NULL,
    claim_id     TEXT        NOT NULL REFERENCES claims(claim_id),
    event_kind   TEXT        NOT NULL,
    disposition  TEXT        NOT NULL,
    rationale    JSONB,                     -- DIVERGENCE: JSONB (not TEXT)
    recorded_at  TEXT        NOT NULL,
    stream_seq   BIGINT      NOT NULL,      -- PG-ONLY: OCC monotonic sequence per agent_id
    UNIQUE (agent_id, stream_seq)           -- PG-ONLY: OCC constraint (A41)
);

-- ── CLAIM EDGES ───────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS claim_edges (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    edge_id       TEXT        NOT NULL UNIQUE,
    agent_id      TEXT        NOT NULL,
    from_claim_id TEXT        NOT NULL REFERENCES claims(claim_id),
    to_claim_id   TEXT        NOT NULL REFERENCES claims(claim_id),
    edge_kind     TEXT        NOT NULL,
    created_at    TEXT        NOT NULL,
    UNIQUE (agent_id, from_claim_id, to_claim_id, edge_kind)  -- identical to SQLite A26
);

-- ── INDEXES ───────────────────────────────────────────────────────────────────
-- Covering index: (agent_id, subject, predicate, tx_time DESC) for load_subject_line
CREATE INDEX IF NOT EXISTS idx_claims_subject_line
    ON claims (agent_id, subject, predicate, tx_time DESC);

CREATE INDEX IF NOT EXISTS idx_validity_assertions_target
    ON validity_assertions (agent_id, target_claim_id, asserted_at DESC);

CREATE INDEX IF NOT EXISTS idx_ledger_agent_time
    ON ledger_entries (agent_id, recorded_at DESC);

CREATE INDEX IF NOT EXISTS idx_edges_from
    ON claim_edges (agent_id, from_claim_id, edge_kind);

CREATE INDEX IF NOT EXISTS idx_edges_to
    ON claim_edges (agent_id, to_claim_id, edge_kind);

CREATE INDEX IF NOT EXISTS idx_claims_provenance
    ON claims (agent_id, provenance_label, tx_time DESC);
