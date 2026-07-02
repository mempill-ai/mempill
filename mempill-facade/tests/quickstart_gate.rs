//! CI-enforced regression gate for `examples/quickstart.rs`.
//!
//! Runs the quickstart example as a subprocess so its own `assert!`/`assert_eq!`
//! calls (Munich succession, Contested value=None + 2 candidates) are wired into
//! `cargo test` rather than left as a standalone example that can silently rot.
//! A panic inside the example propagates as a non-zero exit code, which this
//! test asserts against — so CI fails loudly instead of swallowing it.

use std::process::Command;

#[test]
fn quickstart_example_exits_zero_and_passes() {
    let output = Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--example", "quickstart", "-p", "mempill"])
        .output()
        .expect("failed to spawn `cargo run --example quickstart`");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "quickstart example exited non-zero ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code(),
    );
    assert!(
        stdout.contains("quickstart passed"),
        "quickstart example did not print the success marker\nstdout:\n{stdout}",
    );
}
