# mempill

Temporally-correct memory for AI agents — bi-temporal claim store with Contested-first conflict surfacing and oracle resolution.

📖 **Documentation: <https://mempill.netlify.app>**

This crate is a thin facade over [`mempill-core`](../mempill-core) and the persistence adapters.

## Usage

```toml
[dependencies]
mempill = "0.2"                          # default = SQLite backend
# or:
mempill = { version = "0.2", features = ["postgres"] }
```

## Quick start

```rust
use mempill::{open_default_in_memory, remember, recall, RememberOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = open_default_in_memory()?;

    // Remember a fact — 3 args + sane defaults. Dates are lenient: "2020", "2020-03", or RFC3339.
    remember(&engine, "my-agent", "user", "city", "Berlin",
             RememberOptions::default().valid_from("2020")).await?;

    // Conflicting facts are NEVER silently overwritten — they surface as Contested.
    remember(&engine, "my-agent", "acme:ceo", "held_by", "Alice", RememberOptions::default()).await?;
    remember(&engine, "my-agent", "acme:ceo", "held_by", "Bob",   RememberOptions::default()).await?;

    // Recall — flat result; Contested is explicit (can't be mistaken for "no memory").
    let r = recall(&engine, "my-agent", "acme:ceo", "held_by").await?;
    if r.is_contested() {
        println!("contested: {:?}", r.candidates);
    } else {
        println!("ceo = {:?}", r.as_str());
    }
    Ok(())
}
```

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `sqlite` | yes | Embedded SQLite adapter (topology-a, file-per-agent) |
| `postgres` | no | Shared PostgreSQL adapter (topology-b, r2d2 pool, advisory locking) |

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
