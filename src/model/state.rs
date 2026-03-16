use serde::{Deserialize, Serialize};
use std::num::NonZeroI32;

use super::ids::ProcessIdentity;

/// Current status of a run. State-specific fields live inside the variants,
/// making invalid combinations unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum RunStatus {
    /// Sidecar started, child not yet spawned.
    Starting,
    /// Child is alive and being supervised.
    Running { child: ProcessIdentity },
    /// Child failed to spawn. No child identity exists.
    SpawnFailed { ended_at: String },
    /// Run ended after child was running. Child identity preserved.
    Exited {
        child: ProcessIdentity,
        #[serde(flatten)]
        how: ExitReason,
        ended_at: String,
    },
    /// Sidecar disappeared without writing terminal state.
    /// May or may not have had a child.
    SidecarLost {
        child: Option<ProcessIdentity>,
        ended_at: String,
    },
}

/// How a running child exited. Only reachable from Running state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason")]
pub enum ExitReason {
    /// Child exited with code 0.
    ExitedOk,
    /// Child exited with non-zero code.
    ExitedError {
        #[serde(with = "nonzero_i32_serde")]
        code: NonZeroI32,
    },
    /// Child was killed gracefully (SIGTERM / cooperative shutdown).
    Killed,
    /// Child was force-killed (SIGKILL / TerminateJobObject).
    KilledForced,
    /// Child exceeded --timeout.
    TimedOut,
}

impl RunStatus {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, RunStatus::Starting | RunStatus::Running { .. })
    }

    /// Get child identity if available.
    pub fn child(&self) -> Option<&ProcessIdentity> {
        match self {
            RunStatus::Starting | RunStatus::SpawnFailed { .. } => None,
            RunStatus::Running { child } | RunStatus::Exited { child, .. } => Some(child),
            RunStatus::SidecarLost { child, .. } => child.as_ref(),
        }
    }

    /// Get ended_at if terminal.
    pub fn ended_at(&self) -> Option<&str> {
        match self {
            RunStatus::Starting | RunStatus::Running { .. } => None,
            RunStatus::SpawnFailed { ended_at }
            | RunStatus::Exited { ended_at, .. }
            | RunStatus::SidecarLost { ended_at, .. } => Some(ended_at),
        }
    }
}

/// serde helper for NonZeroI32.
mod nonzero_i32_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::num::NonZeroI32;

    pub fn serialize<S>(value: &NonZeroI32, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i32(value.get())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NonZeroI32, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = i32::deserialize(deserializer)?;
        NonZeroI32::new(value).ok_or_else(|| serde::de::Error::custom("exit code cannot be zero"))
    }
}
