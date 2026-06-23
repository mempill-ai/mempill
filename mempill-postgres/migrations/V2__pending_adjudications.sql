CREATE TABLE IF NOT EXISTS pending_adjudications (
    handle_id            TEXT NOT NULL PRIMARY KEY,
    agent_id             TEXT NOT NULL,
    subject              TEXT NOT NULL,
    predicate            TEXT NOT NULL,
    challenger_claim_ref TEXT NOT NULL,
    incumbent_claim_ref  TEXT NOT NULL,
    request_payload      TEXT NOT NULL,            -- JSON-encoded AdjudicationRequest
    queued_at            TIMESTAMPTZ NOT NULL,
    expires_at           TIMESTAMPTZ,              -- NULL = no TTL
    status               TEXT NOT NULL DEFAULT 'pending'  -- 'pending' | 'resolved' | 'expired'
);
CREATE INDEX IF NOT EXISTS idx_pending_adj_agent_id ON pending_adjudications(agent_id);
CREATE INDEX IF NOT EXISTS idx_pending_adj_expires_at ON pending_adjudications(expires_at)
    WHERE expires_at IS NOT NULL AND status = 'pending';
