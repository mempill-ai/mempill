//! Compile-level proof for F-01: `mempill::postgres::PoolConfig` must be nameable
//! and constructible through the facade crate, without a direct `mempill-postgres`
//! dependency and without a live database connection.
#![cfg(feature = "postgres")]

use mempill::postgres::PoolConfig;

#[test]
fn pool_config_is_reexported_and_constructible() {
    let config = PoolConfig {
        max_size: 50,
        connection_timeout: std::time::Duration::from_secs(10),
    };
    assert_eq!(config.max_size, 50);
    assert_eq!(config.connection_timeout, std::time::Duration::from_secs(10));

    let default_config = PoolConfig::default();
    assert_eq!(default_config, PoolConfig::default());
}
