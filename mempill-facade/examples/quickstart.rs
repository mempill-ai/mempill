//! # quickstart — 5-minute mempill introduction
//!
//! Demonstrates the Tier-1 ergonomic API (`remember` / `recall`).
//! Zero internal-type imports required.
//!
//! Run with:
//!   cargo run -p mempill --example quickstart

use mempill::{open_default_in_memory, remember, recall, RememberOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = open_default_in_memory()?;
    let agent  = "my-agent";
    remember(&engine, agent, "user", "city", "Berlin",
        RememberOptions::new().valid_from("2020-01-01").valid_until("2025-01-01")).await?;
    remember(&engine, agent, "user", "city", "Munich",
        RememberOptions::new().valid_from("2025-01-01")).await?;
    let result = recall(&engine, agent, "user", "city").await?;
    assert_eq!(result.as_str(), Some("Munich"));
    assert!(!result.is_contested());
    remember(&engine, agent, "acme", "ceo", "Alice", RememberOptions::new()).await?;
    remember(&engine, agent, "acme", "ceo", "Bob",   RememberOptions::new()).await?;
    let ceo = recall(&engine, agent, "acme", "ceo").await?;
    assert!(ceo.is_contested());
    assert_eq!(ceo.candidates.len(), 2);
    assert!(ceo.value.is_none());
    println!("quickstart passed");
    Ok(())
}
