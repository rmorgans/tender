use serde::{Deserialize, Serialize};

/// Why a session failed during the dependency-wait phase.
/// Machine-readable discriminator inside `DependencyFailed` state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dep_reason")]
pub enum DepFailReason {
    /// Dependency exited non-zero, was not found, or was replaced.
    Failed,
    /// Timeout expired during dependency wait (before child spawn).
    TimedOut,
    /// User-initiated kill during dependency wait (before child spawn).
    Killed,
}
