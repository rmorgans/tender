use thiserror::Error;

use super::ids::{EpochTimestamp, ProcessIdentity};
use super::meta::Meta;
use super::state::{ExitReason, RunStatus};

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("cannot transition from {from} — already terminal")]
    AlreadyTerminal { from: &'static str },
    #[error("illegal transition from {from} to {to}")]
    Illegal {
        from: &'static str,
        to: &'static str,
    },
}

fn status_name(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Starting => "Starting",
        RunStatus::Running { .. } => "Running",
        RunStatus::SpawnFailed { .. } => "SpawnFailed",
        RunStatus::Exited { how, .. } => match how {
            ExitReason::ExitedOk => "ExitedOk",
            ExitReason::ExitedError { .. } => "ExitedError",
            ExitReason::Killed => "Killed",
            ExitReason::KilledForced => "KilledForced",
            ExitReason::TimedOut => "TimedOut",
        },
        RunStatus::SidecarLost { .. } => "SidecarLost",
    }
}

impl Meta {
    /// Transition Starting → Running. Requires child identity.
    pub fn transition_running(&mut self, child: ProcessIdentity) -> Result<(), TransitionError> {
        match self.status() {
            RunStatus::Starting => {
                *self.status_mut() = RunStatus::Running { child };
                Ok(())
            }
            RunStatus::Running { .. } => Err(TransitionError::Illegal {
                from: "Running",
                to: "Running",
            }),
            _ => Err(TransitionError::AlreadyTerminal {
                from: status_name(self.status()),
            }),
        }
    }

    /// Transition Starting → SpawnFailed. Child never started.
    /// Only valid from Starting — cannot reach SpawnFailed from Running.
    pub fn transition_spawn_failed(
        &mut self,
        ended_at: EpochTimestamp,
    ) -> Result<(), TransitionError> {
        match self.status() {
            RunStatus::Starting => {
                *self.status_mut() = RunStatus::SpawnFailed { ended_at };
                Ok(())
            }
            RunStatus::Running { .. } => Err(TransitionError::Illegal {
                from: "Running",
                to: "SpawnFailed",
            }),
            _ => Err(TransitionError::AlreadyTerminal {
                from: status_name(self.status()),
            }),
        }
    }

    /// Transition Running → Exited. Only valid from Running.
    /// ExitReason cannot include SpawnFailed — that's a separate type.
    /// Child identity is carried from Running into the Exited state.
    pub fn transition_exited(
        &mut self,
        how: ExitReason,
        ended_at: EpochTimestamp,
    ) -> Result<(), TransitionError> {
        match self.status() {
            RunStatus::Running { child } => {
                let child = *child;
                *self.status_mut() = RunStatus::Exited {
                    child,
                    how,
                    ended_at,
                };
                Ok(())
            }
            RunStatus::Starting => Err(TransitionError::Illegal {
                from: "Starting",
                to: "Exited",
            }),
            _ => Err(TransitionError::AlreadyTerminal {
                from: status_name(self.status()),
            }),
        }
    }

    /// Reconciliation: mark as SidecarLost. The ONLY case where
    /// something other than the sidecar writes lifecycle state.
    /// Valid from Starting (no child) or Running (with child).
    pub fn reconcile_sidecar_lost(
        &mut self,
        ended_at: EpochTimestamp,
    ) -> Result<(), TransitionError> {
        match self.status() {
            RunStatus::Starting => {
                *self.status_mut() = RunStatus::SidecarLost {
                    child: None,
                    ended_at,
                };
                Ok(())
            }
            RunStatus::Running { child } => {
                let child = *child;
                *self.status_mut() = RunStatus::SidecarLost {
                    child: Some(child),
                    ended_at,
                };
                Ok(())
            }
            _ => Err(TransitionError::AlreadyTerminal {
                from: status_name(self.status()),
            }),
        }
    }
}
