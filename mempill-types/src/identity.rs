//! Identity types: opaque, stable identifiers used as primary keys and partition keys.

/// Opaque stable identifier for a memory agent. Primary partition key everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub String);

/// Opaque, stable, immutable identity of a committed claim. Minted once at injection time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ClaimRef(pub uuid::Uuid);

impl ClaimRef {
    /// Mint a new random ClaimRef.
    pub fn new_random() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Compound key identifying the (agent_id, subject, predicate) subject-line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SubjectLineRef {
    /// The agent that owns this subject-line.
    pub agent_id: AgentId,
    /// The entity being described.
    pub subject: String,
    /// The aspect being asserted.
    pub predicate: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_equality() {
        let a = AgentId("agent-1".into());
        let b = AgentId("agent-1".into());
        assert_eq!(a, b);
    }

    #[test]
    fn claim_ref_new_random_is_unique() {
        let r1 = ClaimRef::new_random();
        let r2 = ClaimRef::new_random();
        assert_ne!(r1, r2);
    }

    #[test]
    fn claim_ref_round_trip_serde() {
        let r = ClaimRef::new_random();
        let json = serde_json::to_string(&r).unwrap();
        let back: ClaimRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn claim_ref_serializes_as_bare_uuid_string() {
        let uuid = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let r = ClaimRef(uuid);
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#""550e8400-e29b-41d4-a716-446655440000""#);
        let back: ClaimRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn agent_id_serializes_as_bare_string() {
        let id = AgentId("my-agent".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""my-agent""#);
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn subject_line_ref_equality() {
        let s1 = SubjectLineRef {
            agent_id: AgentId("a".into()),
            subject: "user".into(),
            predicate: "name".into(),
        };
        let s2 = s1.clone();
        assert_eq!(s1, s2);
    }
}
