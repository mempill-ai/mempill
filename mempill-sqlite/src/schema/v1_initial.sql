-- mempill-sqlite/src/schema/v1_initial.sql
-- Append-only schema. No UPDATE or DELETE on claims, validity_assertions, ledger_entries,
-- or claim_edges. Violations are schema-enforced and tested (I1, DC-2).
-- PRAGMA journal_mode=WAL set at connection open (not DDL) — applied in connection.rs (W5).
-- PRAGMA synchronous=FULL set at connection open (DC-D, CONSTRAINTS.md §D) — applied in connection.rs (W5).

-- ── CLAIMS ────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS claims (
    claim_id            TEXT    NOT NULL,   -- UUID; immutable PK
    agent_id            TEXT    NOT NULL,   -- partition key; first column always (A12)
    subject             TEXT    NOT NULL,
    predicate           TEXT    NOT NULL,
    value               TEXT    NOT NULL,   -- JSON-encoded; serde_json::Value
    cardinality         TEXT    NOT NULL DEFAULT 'Unknown',  -- 'Functional'|'SetValued'|'Unknown'

    -- Provenance (I4, DC-1): immutable after INSERT. No UPDATE path exists.
    provenance_label    TEXT    NOT NULL,   -- 'External_UserAsserted'|'External_ExternalFirstHand'|'RecallReEntry'|'ModelDerived'
    nearest_external_anchor_id TEXT,       -- FK to claims.claim_id; NULL for first-hand external
    derivation_depth    INTEGER NOT NULL DEFAULT 0,  -- OP-1 cap tracking

    -- Bi-temporal axes (I2, A3). tx_time is engine-stamped; never host-supplied.
    tx_time             TEXT    NOT NULL,   -- ISO-8601 UTC; engine-assigned
    valid_time_start    TEXT,               -- ISO-8601 UTC; NULL = unknown (fallible, F4)
    valid_time_end      TEXT,               -- ISO-8601 UTC; NULL = open / unknown
    valid_time_confidence REAL  NOT NULL DEFAULT 0.0,

    -- Confidence (two separate scores, SDK_CONTRACT §1.4)
    value_confidence    REAL    NOT NULL DEFAULT 0.5,

    -- Criticality (V3-7): distinct from currency; surfaced at read
    criticality         TEXT    NOT NULL DEFAULT 'Medium',  -- 'Low'|'Medium'|'High'|'Critical'

    -- Lineage (JSON array of ClaimRef UUIDs; secondary to claim_edges table)
    derived_from        TEXT    NOT NULL DEFAULT '[]',  -- JSON array

    -- Reserved for future snapshot/compaction (TECH_STACK.md §5 risk #2)
    metadata            TEXT,               -- JSON; NULL in v0.1
    snapshot_schema_version INTEGER,        -- NULL in v0.1

    -- For vector index model-swap safety (CONSTRAINTS.md §D, A10)
    embedding_model_id  TEXT,               -- NULL until vector is enabled

    PRIMARY KEY (claim_id)
    -- Note: no UNIQUE constraint prevents two claims with same (agent_id, subject, predicate, value)
    -- because the idempotency check (I6) is enforced in Rust logic, not schema.
    -- Two semantically-identical claims may legitimately have different ProvenanceLabel.
);

-- ── VALIDITY ASSERTIONS ───────────────────────────────────────────────────────
-- Separate table (not embedded in claims) so the history of bounds/reopens is independently queryable.
CREATE TABLE IF NOT EXISTS validity_assertions (
    assertion_id        TEXT    NOT NULL,   -- UUID
    agent_id            TEXT    NOT NULL,   -- partition key
    target_claim_id     TEXT    NOT NULL,   -- FK to claims.claim_id
    assertion_kind      TEXT    NOT NULL,   -- 'Bound'|'Reopen'
    bound_at            TEXT,               -- ISO-8601 UTC; for 'Bound' kind
    reopen_at           TEXT,               -- ISO-8601 UTC; for 'Reopen' kind
    provenance_label    TEXT    NOT NULL,
    value_confidence    REAL    NOT NULL DEFAULT 0.5,
    valid_time_confidence REAL  NOT NULL DEFAULT 0.5,
    asserted_at         TEXT    NOT NULL,   -- engine-stamped tx_time of assertion

    PRIMARY KEY (assertion_id),
    FOREIGN KEY (target_claim_id) REFERENCES claims(claim_id)
);

-- ── LEDGER ENTRIES ────────────────────────────────────────────────────────────
-- Append-only audit trail. Every decision is recorded (C8, G1 replay determinism).
CREATE TABLE IF NOT EXISTS ledger_entries (
    entry_id            TEXT    NOT NULL,   -- UUID
    agent_id            TEXT    NOT NULL,   -- partition key
    claim_id            TEXT    NOT NULL,
    event_kind          TEXT    NOT NULL,   -- LedgerEventKind enum value
    disposition         TEXT    NOT NULL,   -- Disposition enum value
    rationale           TEXT,               -- JSON; C7 measured estimators + rationale
    recorded_at         TEXT    NOT NULL,   -- engine-stamped tx_time

    PRIMARY KEY (entry_id),
    FOREIGN KEY (claim_id) REFERENCES claims(claim_id)
);

-- ── CLAIM EDGES ───────────────────────────────────────────────────────────────
-- Adjacency table for lineage, supersession, depends-on, mutual-exclusion overlays (B2, V3-8, OP-1).
-- Recursive CTEs traverse this table for lineage queries.
CREATE TABLE IF NOT EXISTS claim_edges (
    edge_id             TEXT    NOT NULL,   -- UUID
    agent_id            TEXT    NOT NULL,   -- partition key
    from_claim_id       TEXT    NOT NULL,
    to_claim_id         TEXT    NOT NULL,
    edge_kind           TEXT    NOT NULL,   -- 'DerivedFrom'|'Supersedes'|'DependsOn'|'MutualExclusion'
    created_at          TEXT    NOT NULL,   -- engine-stamped tx_time

    PRIMARY KEY (edge_id),
    FOREIGN KEY (from_claim_id) REFERENCES claims(claim_id),
    FOREIGN KEY (to_claim_id)   REFERENCES claims(claim_id)
);
