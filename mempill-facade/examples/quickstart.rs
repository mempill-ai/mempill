//! # quickstart — 5-minute mempill introduction
//!
//! Demonstrates the Tier-1 ergonomic API (`remember` / `recall`).
//! Zero internal-type imports required.
//!
//! Run with:
//!   cargo run -p mempill --example quickstart

use mempill::{open_default_in_memory, recall, remember, RememberOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = open_default_in_memory()?;
    let agent = "my-agent";

    // ── Step 1: remember two facts with non-overlapping temporal windows ───────
    // "user/city = Berlin"  valid [2020-01-01, 2025-01-01)
    remember(
        &engine,
        agent,
        "user",
        "city",
        "Berlin",
        RememberOptions::new()
            .valid_from("2020-01-01")
            .valid_until("2025-01-01"),
    )
    .await?;

    // "user/city = Munich"  valid [2025-01-01, ∞)
    // Munich's window is open-ended from 2025 → covers NOW (2026), so it wins.
    remember(
        &engine,
        agent,
        "user",
        "city",
        "Munich",
        RememberOptions::new().valid_from("2025-01-01"),
    )
    .await?;

    // ── Step 2: recall — Munich is the live value (succession, not conflict) ───
    let result = recall(&engine, agent, "user", "city").await?;
    assert_eq!(result.as_str(), Some("Munich"), "expected Munich to be the live value");
    assert!(!result.is_contested(), "expected Resolved, not Contested");
    println!("city = {:?}  (is_contested={})", result.as_str(), result.is_contested());

    // ── Step 3: Contested — two timeless facts for the same functional line ───
    remember(&engine, agent, "acme", "ceo", "Alice", RememberOptions::new()).await?;
    remember(&engine, agent, "acme", "ceo", "Bob", RememberOptions::new()).await?;

    let ceo = recall(&engine, agent, "acme", "ceo").await?;
    assert!(ceo.is_contested(), "expected Contested after two timeless conflicting facts");
    assert_eq!(ceo.candidates.len(), 2, "both Alice and Bob must surface as candidates");
    assert!(ceo.value.is_none(), "Contested: value is None — use candidates, not value");

    println!(
        "acme/ceo: is_contested={}, value={:?}, candidates={:?}",
        ceo.is_contested(),
        ceo.value,
        ceo.candidates.iter().map(|c| &c.value).collect::<Vec<_>>(),
    );

    println!("quickstart passed");
    Ok(())
}
