//! Per-agent_id async write lock.
//!
//! Enforces the single-writer-per-agent_id invariant at the async task layer:
//! at most one Tokio task may hold write authority for a given agent_id at any time.
//!
//! Implementation:
//! - Uses `tokio::sync::OwnedMutexGuard` obtained via `lock_owned()` — no unsafe.
//! - `Arc<Mutex<()>>` per agent_id; map protected by `RwLock<HashMap<...>>`.
//! - `clone()`-able: `EngineHandle` can be cloned and all clones share the same lock map.
//!
//! For single-process embedded SQLite, this Tokio lock is the sole enforcement layer.
//! The PostgreSQL adapter additionally uses `pg_try_advisory_lock(hashtext(agent_id))`
//! for cross-process enforcement; the in-process lock remains in both cases.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use mempill_types::AgentId;

/// Shared, cloneable map of per-agent_id async write locks.
///
/// Acquiring the lock for a given `agent_id` serializes all write operations
/// for that agent within this process. Different agent_ids proceed independently.
#[derive(Clone)]
pub struct AgentWriteLockMap {
    locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
}

impl AgentWriteLockMap {
    /// Create a new, empty lock map.
    pub fn new() -> Self {
        Self {
            locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Acquire the write lock for `agent_id`.
    ///
    /// Returns an `OwnedMutexGuard` that releases the lock on drop.
    /// If the lock is already held (by another task), this call parks the current
    /// task until the lock is released — tasks are never rejected, only serialized.
    ///
    /// Uses `lock_owned()` so the guard is `'static` — no lifetime coupling to the
    /// map or any intermediate `Arc`.
    pub async fn acquire(&self, agent_id: &AgentId) -> OwnedMutexGuard<()> {
        let lock = {
            let read = self.locks.read().await;
            read.get(&agent_id.0).cloned()
        };

        let lock = match lock {
            Some(l) => l,
            None => {
                // Entry missing — take write lock on the map and insert.
                let mut write = self.locks.write().await;
                // Re-check under write lock (another task may have inserted between our
                // read and write lock acquisition).
                write
                    .entry(agent_id.0.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            }
        };

        // `lock_owned()` returns an OwnedMutexGuard<()> tied to the Arc, not to
        // any reference with a shorter lifetime — no unsafe required.
        lock.lock_owned().await
    }
}

impl Default for AgentWriteLockMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn same_agent_id_locks_are_mutually_exclusive() {
        // Acquire lock for agent-A, then verify a second acquire for the same agent-A
        // cannot proceed until the first guard is dropped.
        let map = AgentWriteLockMap::new();
        let agent_a = AgentId("agent-A".into());
        let counter = Arc::new(AtomicUsize::new(0));

        // First acquire — held for the duration of the test body.
        let _guard1 = map.acquire(&agent_a).await;
        counter.fetch_add(1, Ordering::SeqCst);

        let map2 = map.clone();
        let agent_a2 = agent_a.clone();
        let counter2 = counter.clone();

        // Spawn a task that tries to acquire the same lock.
        // With the first guard held, it should block.
        let handle = tokio::spawn(async move {
            let _guard2 = map2.acquire(&agent_a2).await;
            counter2.fetch_add(10, Ordering::SeqCst);
        });

        // Give the spawned task time to reach the lock acquisition — it must NOT proceed.
        tokio::time::sleep(Duration::from_millis(20)).await;
        // If the second task had acquired the lock, counter would be 11.
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "second acquire must be blocked while first guard is held"
        );

        // Drop first guard — second task can now proceed.
        drop(_guard1);
        timeout(Duration::from_millis(200), handle)
            .await
            .expect("task timed out")
            .expect("task panicked");

        assert_eq!(
            counter.load(Ordering::SeqCst),
            11,
            "after releasing first guard, second acquire must complete"
        );
    }

    #[tokio::test]
    async fn different_agent_ids_proceed_independently() {
        // Two different agent_ids must be acquirable concurrently without blocking each other.
        let map = AgentWriteLockMap::new();
        let agent_a = AgentId("agent-A".into());
        let agent_b = AgentId("agent-B".into());

        let _guard_a = map.acquire(&agent_a).await;
        // agent-B's lock is entirely independent — must not block.
        let acquire_b = timeout(Duration::from_millis(100), map.acquire(&agent_b)).await;
        assert!(
            acquire_b.is_ok(),
            "acquiring lock for agent-B must not block even when agent-A lock is held"
        );
    }

    #[tokio::test]
    async fn lock_map_is_cloneable_and_shares_state() {
        // A clone of the map shares the same underlying lock state.
        let map = AgentWriteLockMap::new();
        let map_clone = map.clone();
        let agent = AgentId("agent-clone-test".into());

        let _guard = map.acquire(&agent).await;

        // Cloned map should also see the lock as taken.
        let acquire_via_clone = timeout(
            Duration::from_millis(30),
            map_clone.acquire(&agent),
        )
        .await;

        assert!(
            acquire_via_clone.is_err(),
            "cloned map must share lock state — second acquire via clone must block"
        );
    }

    #[tokio::test]
    async fn guard_drops_release_the_lock() {
        let map = AgentWriteLockMap::new();
        let agent = AgentId("agent-drop-test".into());

        {
            let _guard = map.acquire(&agent).await;
            // guard dropped here
        }

        // After drop, the lock is released and can be acquired again.
        let reacquire = timeout(Duration::from_millis(50), map.acquire(&agent)).await;
        assert!(reacquire.is_ok(), "lock must be released after guard drop");
    }

    #[tokio::test]
    async fn multiple_agents_independently_concurrent() {
        // Acquire locks for N distinct agents concurrently — none should block each other.
        let map = AgentWriteLockMap::new();
        let mut handles = vec![];

        for i in 0..8 {
            let map_i = map.clone();
            let agent_i = AgentId(format!("agent-{i}"));
            handles.push(tokio::spawn(async move {
                let _g = map_i.acquire(&agent_i).await;
                // Hold briefly to ensure concurrent execution.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
        }

        let results = futures::future::join_all(handles).await;
        for (i, r) in results.into_iter().enumerate() {
            r.unwrap_or_else(|e| panic!("agent-{i} lock task panicked: {e}"));
        }
    }
}
