//! PostgresPersistenceStore: r2d2 pool construction + refinery migration bootstrap.
//!
//! ## Connection string format
//!
//! Accepts libpq-style key-value strings, for example:
//! ```text
//! host=localhost port=5432 user=mempill dbname=mempill password=secret
//! ```
//! Or a URI: `postgresql://mempill:secret@localhost:5432/mempill`.
//!
//! ## TLS
//!
//! v0.3 uses `NoTls` (suitable for local Docker / CI environments).
//! // v0.3.1: add TlsMode param to accept a `native_tls::TlsConnector` for cloud Postgres (RDS, Neon, Supabase).
//!
//! ## Pool settings
//!
//! - `max_size = 20` connections (default)
//! - `connection_timeout = 5s` (default)
//!
//! Both are configurable via [`PoolConfig`] and [`PostgresPersistenceStore::with_pool_config`].

use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;
use postgres::NoTls;

/// Default r2d2 pool max size, matching v0.3 hardcoded behavior.
const DEFAULT_MAX_SIZE: u32 = 20;

/// Default r2d2 pool connection timeout, matching v0.3 hardcoded behavior.
const DEFAULT_CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Error type for PostgresPersistenceStore operations.
#[derive(Debug, thiserror::Error)]
pub enum PostgresStoreError {
    #[error("postgres driver error: {0}")]
    Postgres(#[from] postgres::Error),

    #[error("r2d2 pool error: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("refinery migration error: {0}")]
    Migration(#[from] refinery::Error),

    #[error("domain mapping error: {0}")]
    Mapping(String),

    #[error("invalid pool configuration: {0}")]
    Config(String),
}

/// Configuration for the underlying r2d2 connection pool.
///
/// Construct via [`PoolConfig::default`] and override individual fields, e.g.:
///
/// ```
/// use mempill_postgres::PoolConfig;
///
/// let config = PoolConfig {
///     max_size: 50,
///     connection_timeout: std::time::Duration::from_secs(10),
/// };
/// ```
///
/// `max_size` must be `>= 1`; validated by [`PostgresPersistenceStore::with_pool_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// Maximum number of pooled connections. Must be `>= 1`.
    pub max_size: u32,

    /// How long to wait for a connection to become available before erroring.
    pub connection_timeout: std::time::Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_SIZE,
            connection_timeout: DEFAULT_CONNECTION_TIMEOUT,
        }
    }
}

/// The PostgreSQL-backed `PersistencePort` implementation.
///
/// Construct via [`PostgresPersistenceStore::new`] (default pool settings) or
/// [`PostgresPersistenceStore::with_pool_config`] (custom pool size / timeout).
/// Clone-friendly: the inner `r2d2::Pool` is `Arc`-wrapped by r2d2.
pub struct PostgresPersistenceStore {
    pub(crate) pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PostgresPersistenceStore {
    /// Bootstrap entry point: run refinery migrations on a dedicated connection,
    /// then build the r2d2 connection pool using default pool settings
    /// (`max_size = 20`, `connection_timeout = 5s`).
    ///
    /// # Errors
    ///
    /// Returns `PostgresStoreError` if the connection string is invalid, the DB
    /// is unreachable, migrations fail, or the pool cannot be built.
    pub fn new(connection_string: &str) -> Result<Self, PostgresStoreError> {
        Self::with_pool_config(connection_string, PoolConfig::default())
    }

    /// Same as [`PostgresPersistenceStore::new`], but with a caller-supplied
    /// [`PoolConfig`] instead of the defaults.
    ///
    /// # Errors
    ///
    /// Returns `PostgresStoreError::Config` if `pool_config.max_size == 0`.
    /// Returns `PostgresStoreError` if the connection string is invalid, the DB
    /// is unreachable, migrations fail, or the pool cannot be built.
    pub fn with_pool_config(
        connection_string: &str,
        pool_config: PoolConfig,
    ) -> Result<Self, PostgresStoreError> {
        if pool_config.max_size == 0 {
            return Err(PostgresStoreError::Config(format!(
                "pool max_size must be >= 1, got {}",
                pool_config.max_size
            )));
        }

        // 1. Dedicated migration connection (not from pool — avoids pool startup contention).
        let mut mig_client = postgres::Client::connect(connection_string, NoTls)?;
        crate::migrations::runner().run(&mut mig_client)?;
        drop(mig_client);

        // 2. Build the r2d2 pool.
        let manager = PostgresConnectionManager::new(
            connection_string.parse()?,
            NoTls,
        );
        let pool = r2d2::Pool::builder()
            .max_size(pool_config.max_size)
            .connection_timeout(pool_config.connection_timeout)
            .build(manager)?;

        Ok(Self { pool })
    }

    /// Returns the effective `max_size` of the underlying r2d2 pool.
    ///
    /// Exposed primarily for tests that verify a supplied [`PoolConfig`] was
    /// actually applied to the pool, rather than merely accepted at construction.
    pub fn pool_max_size(&self) -> u32 {
        self.pool.max_size()
    }

    /// Returns the effective `connection_timeout` of the underlying r2d2 pool.
    ///
    /// Exposed primarily for tests that verify a supplied [`PoolConfig`] was
    /// actually applied to the pool, rather than merely accepted at construction.
    pub fn pool_connection_timeout(&self) -> std::time::Duration {
        self.pool.connection_timeout()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pool_config_matches_v0_3_hardcoded_values() {
        let config = PoolConfig::default();
        assert_eq!(config.max_size, 20);
        assert_eq!(config.connection_timeout, std::time::Duration::from_secs(5));
    }

    #[test]
    fn pool_config_zero_max_size_is_rejected_before_touching_the_network() {
        // No live DB required: max_size validation happens before the connection
        // attempt, so this must fail fast with a Config error, not a Postgres error.
        let result = PostgresPersistenceStore::with_pool_config(
            "host=127.0.0.1 port=1 user=nobody dbname=nowhere",
            PoolConfig {
                max_size: 0,
                connection_timeout: std::time::Duration::from_secs(5),
            },
        );
        match result {
            Err(PostgresStoreError::Config(msg)) => {
                assert!(msg.contains("max_size"));
            }
            Err(other) => panic!("expected PostgresStoreError::Config, got a different error: {other}"),
            Ok(_) => panic!("expected PostgresStoreError::Config, got Ok(..)"),
        }
    }
}
