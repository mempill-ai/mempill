//! # temporal_validity — two-act mempill demo
//!
//! Demonstrates the two core mempill guarantees:
//!
//! **Act 1 — Contested:** conflicting facts with no temporal resolution are NEVER silently
//! overwritten. mempill surfaces BOTH values and keeps the belief Contested until a human
//! or oracle resolves it.
//!
//! **Act 2 — Temporal succession:** when two claims cover non-overlapping valid-time windows
//! (both annotated with high confidence), mempill treats them as a clean succession — no
//! conflict, no oracle needed. `query_memory` returns the single claim whose window contains
//! the instant you ask about, whether that is NOW or any point in the past.
//!
//! Run with:
//!   cargo run -p mempill --example temporal_validity
//!
//! All imports come exclusively from `mempill::` — no direct dependency on
//! mempill-core, mempill-sqlite, or mempill-types is required.

use mempill::{
    // Engine entry points
    open_default_in_memory,
    // Request/response DTOs
    IngestClaimRequest,
    QueryMemoryRequest,
    // Domain value objects
    AgentId,
    Cardinality,
    Confidence,
    Criticality,
    ValidTime,
    // Belief/status types for printing
    BeliefStatus,
    // Provenance
    ExternalKind,
    ProvenanceLabel,
};

fn parse_dt(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn bounded_vt(start: &str, end: &str) -> ValidTime {
    ValidTime {
        start: Some(parse_dt(start)),
        end: Some(parse_dt(end)),
        valid_time_confidence: 0.9, // above the 0.7 succession threshold
        start_granularity: None, end_granularity: None,
    }
}

fn open_vt(start: &str) -> ValidTime {
    ValidTime {
        start: Some(parse_dt(start)),
        end: None, // open-ended: "until further notice"
        valid_time_confidence: 0.9,
        start_granularity: None, end_granularity: None,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = open_default_in_memory()?;
    let agent = AgentId("demo-agent".into());

    println!("=== mempill temporal_validity demo ===");
    println!();

    // ════════════════════════════════════════════════════════════════════════════
    // ACT 1 — CONTESTED: never silently overwrite a genuine conflict
    // ════════════════════════════════════════════════════════════════════════════

    println!("──── Act 1: Contested (no valid-time, no oracle) ────────────────────────");
    println!();
    println!("Scenario: two sources disagree on who is Acme's CEO.");
    println!("Neither claim carries a valid-time window, so mempill cannot");
    println!("determine which is more recent or which period each covers.");
    println!();

    // First claim: acme / ceo = Alice
    let alice_resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("Alice"),
            provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            cardinality: Cardinality::Functional,
            valid_time: None, // no temporal annotation
            confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
            criticality: Criticality::High,
            derived_from: vec![],
        })
        .await?;

    println!(
        "  [ingest] acme/ceo = \"Alice\"  →  disposition: {:?}",
        alice_resp.disposition
    );

    // Second claim: acme / ceo = Bob — same subject+predicate, different value, no valid-time
    let bob_resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("Bob"),
            provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            cardinality: Cardinality::Functional,
            valid_time: None, // still no temporal annotation
            confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
            criticality: Criticality::High,
            derived_from: vec![],
        })
        .await?;

    println!(
        "  [ingest] acme/ceo = \"Bob\"    →  disposition: {:?}",
        bob_resp.disposition
    );
    println!();

    // Query the belief for acme/ceo
    let q_contested = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            as_of_tx_time: None,
        valid_at: None,
        })
        .await?;

    println!("  [query]  acme/ceo  →  belief status: {:?}", q_contested.belief.status);

    // Collect all surfaced values (primary + alternatives)
    let contested_values: Vec<String> = {
        let mut vals: Vec<String> = q_contested
            .belief
            .primary
            .iter()
            .map(|b| format!("\"{}\" (primary)", b.fact.value.as_str().unwrap_or("?")))
            .collect();
        vals.extend(
            q_contested
                .belief
                .alternatives
                .iter()
                .map(|b| format!("\"{}\" (alternative)", b.fact.value.as_str().unwrap_or("?"))),
        );
        vals
    };

    println!("  [query]  surfaced values:");
    for v in &contested_values {
        println!("             - {v}");
    }
    println!();
    println!("  Result: mempill surfaces BOTH Alice and Bob. Neither is silently dropped.");
    println!("  A genuine conflict with no temporal evidence to separate the claims is");
    println!("  held as Contested until a human or oracle resolves it explicitly.");
    println!();

    assert!(
        matches!(
            q_contested.belief.status,
            BeliefStatus::Contested | BeliefStatus::Conflict
        ),
        "Expected Contested/Conflict after two conflicting timeless claims, got {:?}",
        q_contested.belief.status
    );
    assert!(
        contested_values.len() >= 2,
        "Both Alice and Bob must surface in the Contested belief"
    );

    // ════════════════════════════════════════════════════════════════════════════
    // ACT 2 — TEMPORAL SUCCESSION: the valid-time payoff
    // ════════════════════════════════════════════════════════════════════════════

    println!("──── Act 2: Temporal succession (valid-time payoff) ─────────────────────");
    println!();
    println!("Scenario: Globex's CEO changed on 2024-03-01.");
    println!("  Carol held the role from 2020-01-01 to 2024-03-01 (exclusive).");
    println!("  Dave has held it from 2024-03-01 onwards (open-ended).");
    println!("Both claims carry high valid_time_confidence (0.9 ≥ 0.7 threshold).");
    println!("The windows do NOT overlap → mempill recognises a clean succession.");
    println!();

    // Carol: valid [2020-01-01, 2024-03-01)
    let carol_resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "globex".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("Carol"),
            provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            cardinality: Cardinality::Functional,
            valid_time: Some(bounded_vt("2020-01-01T00:00:00Z", "2024-03-01T00:00:00Z")),
            confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.9 },
            criticality: Criticality::High,
            derived_from: vec![],
        })
        .await?;

    println!(
        "  [ingest] globex/ceo = \"Carol\"  valid [2020-01-01, 2024-03-01)  →  {:?}",
        carol_resp.disposition
    );

    // Dave: valid [2024-03-01, ∞)  — non-overlapping successor
    let dave_resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "globex".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("Dave"),
            provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            cardinality: Cardinality::Functional,
            valid_time: Some(open_vt("2024-03-01T00:00:00Z")),
            confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.9 },
            criticality: Criticality::High,
            derived_from: vec![],
        })
        .await?;

    println!(
        "  [ingest] globex/ceo = \"Dave\"   valid [2024-03-01, ∞)           →  {:?}",
        dave_resp.disposition
    );
    println!();
    println!("  Note: Dave's ingest is CommittedCheap (not Contested) — the engine");
    println!("  detected a clean temporal succession and committed both claims.");
    println!();

    assert!(
        matches!(dave_resp.disposition, mempill::Disposition::CommittedCheap),
        "Dave's ingest MUST be CommittedCheap (clean succession), got {:?}",
        dave_resp.disposition
    );

    // ── Query as-of NOW (2026) — falls in Dave's open window ─────────────────
    let q_now = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "globex".into(),
            predicate: "ceo".into(),
            as_of_tx_time: None, // None = use current time
        valid_at: None,
        })
        .await?;

    println!(
        "  [query NOW]          globex/ceo  →  status: {:?}  primary: {}",
        q_now.belief.status,
        q_now
            .belief
            .primary
            .as_ref()
            .map(|b| format!("\"{}\"", b.fact.value.as_str().unwrap_or("?")))
            .unwrap_or_else(|| "none".into())
    );

    assert_eq!(
        q_now.belief.status,
        BeliefStatus::Resolved,
        "Query NOW must be Resolved (Dave's window covers 2026), got {:?}",
        q_now.belief.status
    );
    assert!(
        q_now.belief.alternatives.is_empty(),
        "No alternatives expected in a succession result"
    );
    assert_eq!(
        q_now.belief.primary.as_ref().map(|b| &b.fact.value),
        Some(&serde_json::json!("Dave")),
        "Primary at NOW must be Dave"
    );

    // ── Query as-of 2022-06-01 — falls in Carol's window [2020, 2024-03-01) ──
    let q_past = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "globex".into(),
            predicate: "ceo".into(),
            as_of_tx_time: Some(parse_dt("2022-06-01T00:00:00Z")),
        valid_at: None,
        })
        .await?;

    println!(
        "  [query 2022-06-01]   globex/ceo  →  status: {:?}  primary: {}",
        q_past.belief.status,
        q_past
            .belief
            .primary
            .as_ref()
            .map(|b| format!("\"{}\"", b.fact.value.as_str().unwrap_or("?")))
            .unwrap_or_else(|| "none".into())
    );

    assert_eq!(
        q_past.belief.status,
        BeliefStatus::Resolved,
        "Query 2022-06-01 must be Resolved (Carol's window covers that date), got {:?}",
        q_past.belief.status
    );
    assert_eq!(
        q_past.belief.primary.as_ref().map(|b| &b.fact.value),
        Some(&serde_json::json!("Carol")),
        "Primary at 2022-06-01 must be Carol"
    );

    println!();
    println!("  Result: same subject+predicate, two different time windows.");
    println!("  mempill returns the value that was valid AT the instant you ask about —");
    println!("  no conflict, no oracle, no ambiguity. Temporal correctness by construction.");

    println!();
    println!("=== done ===");

    Ok(())
}
