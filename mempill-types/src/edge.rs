//! ClaimEdge: directed relationship between two claims (lineage, supersession, dependency).

use crate::identity::{AgentId, ClaimRef};
use crate::time::TransactionTime;

/// A directed edge between two claims.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimEdge {
    pub edge_id: uuid::Uuid,
    pub agent_id: AgentId,
    pub from_claim: ClaimRef,
    pub to_claim: ClaimRef,
    pub kind: EdgeKind,
    pub created_at: TransactionTime,
}

/// The semantic relationship carried by a ClaimEdge.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EdgeKind {
    /// `from_claim` was derived from `to_claim` (lineage tracking for provenance depth).
    DerivedFrom,
    /// `to_claim` supersedes `from_claim`.
    Supersedes,
    /// `from_claim` depends on `to_claim` — when `to_claim` is superseded, `from_claim`
    /// is flagged `PendingReview`.
    DependsOn,
    /// Mutual exclusion between from and to (at most one valid at a time,
    /// regardless of cardinality).
    MutualExclusion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentId;
    use chrono::Utc;

    fn make_edge(kind: EdgeKind) -> ClaimEdge {
        ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: AgentId("agent-1".into()),
            from_claim: ClaimRef::new_random(),
            to_claim: ClaimRef::new_random(),
            kind,
            created_at: TransactionTime(Utc::now()),
        }
    }

    #[test]
    fn edge_kind_depends_on_is_present() {
        let e = make_edge(EdgeKind::DependsOn);
        assert_eq!(e.kind, EdgeKind::DependsOn);
    }

    #[test]
    fn all_edge_kinds_round_trip_serde() {
        let kinds = [
            EdgeKind::DerivedFrom,
            EdgeKind::Supersedes,
            EdgeKind::DependsOn,
            EdgeKind::MutualExclusion,
        ];
        for k in &kinds {
            let json = serde_json::to_string(k).unwrap();
            let back: EdgeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, &back);
        }
    }

    #[test]
    fn claim_edge_round_trip_serde() {
        let edge = make_edge(EdgeKind::Supersedes);
        let json = serde_json::to_string(&edge).unwrap();
        let back: ClaimEdge = serde_json::from_str(&json).unwrap();
        assert_eq!(edge.edge_id, back.edge_id);
        assert_eq!(edge.kind, back.kind);
    }
}
