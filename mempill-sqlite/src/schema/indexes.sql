-- mempill-sqlite/src/schema/indexes.sql

-- PRIMARY STRUCTURAL INDEX: covers the canonical fold and subject-line lookup (A10, F8, DC-3).
-- claim retrieval for a (agent_id, subject, predicate) + ordering by tx_time DESC.
CREATE INDEX IF NOT EXISTS idx_claims_subject_line
    ON claims (agent_id, subject, predicate, tx_time DESC);

-- VALIDITY ASSERTIONS: fast lookup by target claim (for fold and supersession queries).
CREATE INDEX IF NOT EXISTS idx_validity_assertions_target
    ON validity_assertions (agent_id, target_claim_id, asserted_at DESC);

-- LEDGER: ordered log replay by agent and time.
CREATE INDEX IF NOT EXISTS idx_ledger_agent_time
    ON ledger_entries (agent_id, recorded_at DESC);

-- CLAIM EDGES: outbound adjacency for lineage traversal.
CREATE INDEX IF NOT EXISTS idx_edges_from
    ON claim_edges (agent_id, from_claim_id, edge_kind);

-- CLAIM EDGES: inbound adjacency for depends-on cascade (V3-8).
CREATE INDEX IF NOT EXISTS idx_edges_to
    ON claim_edges (agent_id, to_claim_id, edge_kind);

-- PROVENANCE: fast lookup of RecallReEntry claims for C6 firewall check (F3).
CREATE INDEX IF NOT EXISTS idx_claims_provenance
    ON claims (agent_id, provenance_label, tx_time DESC);
