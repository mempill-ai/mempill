# mempill (Python)

Python bindings for the mempill AI-agent memory engine — temporal, contested-belief-aware
fact storage for AI agents.

## Install

```sh
pip install mempill
```

Prebuilt wheels are published for Python 3.11, 3.12, and 3.13 on Linux (x86_64, aarch64),
macOS (x86_64, arm64), and Windows (x86_64). **No Rust toolchain required** to use the
published wheel.

### Build from source (contributors)

```sh
pip install maturin
maturin develop --release   # editable install into current venv
```

## Usage

```python
from mempill import open_in_memory, remember, recall

engine = open_in_memory()

remember(engine, "my-agent", "user", "city", "Berlin")
result = recall(engine, "my-agent", "user", "city")
print(result.as_str())        # "Berlin"
print(result.is_contested())  # False
```

### Contested beliefs

When two conflicting claims exist and neither has been reconciled, `recall` signals
a contest rather than silently returning one value:

```python
from mempill import open_in_memory, remember, recall, RememberOptions

engine = open_in_memory()
remember(engine, "agent", "acme", "ceo", "Alice")
remember(engine, "agent", "acme", "ceo", "Bob")   # conflicts → Contested

result = recall(engine, "agent", "acme", "ceo")
if result.is_contested():
    for c in result.candidates:
        print(c.value, c.claim_ref)
```

### Fact history

```python
from mempill import open_in_memory, remember, recall, history, RememberOptions

engine = open_in_memory()
remember(engine, "agent", "acme", "ceo", "Alice",
         RememberOptions(valid_until="2024-01-01"))
remember(engine, "agent", "acme", "ceo", "Bob",
         RememberOptions(valid_from="2024-01-01"))

h = history(engine, "agent", "acme", "ceo")
for entry in h:
    print(entry.value, entry.status, entry.valid_from, entry.valid_until)
```

### File-backed engine

```python
from mempill import open, remember, recall

engine = open("/path/to/agent.db")   # SQLite, persists across restarts
remember(engine, "my-agent", "user", "city", "Berlin")
```

## Exceptions

```
MempillError (base)
  ValidationError   — invalid request fields
  NotFoundError     — claim/agent not found
  ConflictError     — write-authority violation
  StorageError      — database open/migration failure
  ConfigError       — invalid engine configuration
  InternalError     — unexpected engine failure
```

## Type stubs

`.pyi` stubs and `py.typed` marker are included. The package is mypy and stubtest clean.

## Full documentation

https://mempill.netlify.app — concepts, invariants, and the complete API reference.

Source: https://github.com/mempill-ai/mempill

## License

Apache-2.0
