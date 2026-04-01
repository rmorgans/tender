use serde::{Deserialize, Serialize};

/// Who currently controls input to a PTY session.
///
/// Slice one rules (no agent lease):
/// - start --pty → AgentControl
/// - attach → HumanControl (steals from AgentControl)
/// - human detach → AgentControl (always, no lease check)
/// - Detached only used if session started without --stdin
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PtyControl {
    /// Agent owns input — push is accepted.
    AgentControl,
    /// Human is attached — push is rejected, terminal relay is active.
    HumanControl,
    /// No one is connected (no push channel).
    Detached,
}

/// PTY session metadata. Present only for PTY-enabled sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtyMeta {
    pub enabled: bool,
    pub control: PtyControl,
}

impl PtyMeta {
    pub fn new() -> Self {
        Self {
            enabled: true,
            control: PtyControl::AgentControl,
        }
    }
}
