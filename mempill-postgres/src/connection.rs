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
//! - `max_size = 20` connections
//! - `connection_timeout = 5s`

use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;
use postgres::NoTls;

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
}

/// The PostgreSQL-backed `PersistencePort` implementation.
///
/// Construct via [`PostgresPersistenceStore::new`].
/// Clone-friendly: the inner `r2d2::Pool` is `Arc`-wrapped by r2d2.
pub struct PostgresPersistenceStore {
    pub(crate) pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PostgresPersistenceStore {
    /// Bootstrap entry point: run refinery migrations on a dedicated connection,
    /// then build the r2d2 connection pool.
    ///
    /// # Errors
    ///
    /// Returns `PostgresStoreError` if the connection string is invalid, the DB
    /// is unreachable, migrations fail, or the pool cannot be built.
    pub fn new(connection_string: &str) -> Result<Self, PostgresStoreError> {
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
            .max_size(20)
            .connection_timeout(std::time::Duration::from_secs(5))
            .build(manager)?;

        Ok(Self { pool })
    }
}
