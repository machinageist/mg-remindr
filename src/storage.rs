use crate::config::{RedactedDatabaseError, validate_local_database_url};
use std::fmt;

/// Parameterized storage boundary; live PostgreSQL operations are intentionally deferred.
pub trait TodoStore {
    type Error: std::error::Error + Send + Sync + 'static;
    fn execute_parameterized<'a>(
        &'a self,
        statement: &'a str,
        parameters: &'a [&'a (dyn fmt::Debug + Sync)],
    ) -> Result<u64, Self::Error>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresBoundary;
impl PostgresBoundary {
    pub fn connect(database_url: &str) -> Result<Self, RedactedDatabaseError> {
        if validate_local_database_url(database_url).is_err() {
            return Err(crate::config::Config::redacted_database_error());
        }
        Err(crate::config::Config::redacted_database_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundary_validates_before_any_connection() {
        assert!(PostgresBoundary::connect("postgres://localhost.evil/db").is_err());
        assert!(PostgresBoundary::connect("postgres://localhost/db").is_err());
    }
}
