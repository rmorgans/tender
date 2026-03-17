use serde::{Deserialize, Deserializer, Serialize};

use super::ids::{EpochTimestamp, Generation, ProcessIdentity, RunId, SessionName};
use super::spec::LaunchSpec;
use super::state::RunStatus;

/// Persisted session metadata. Schema v1.
/// Written atomically to meta.json via temp file + rename.
///
/// Fields are private — construction only through `new_starting`,
/// mutation only through transition methods in `transition.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    schema_version: u32,
    session: SessionName,
    run_id: RunId,
    generation: Generation,
    launch_spec: LaunchSpec,
    sidecar: ProcessIdentity,
    #[serde(flatten)]
    status: RunStatus,
    started_at: EpochTimestamp,
    restart_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl Meta {
    pub const SCHEMA_VERSION: u32 = 1;

    /// Create initial meta for a new run in Starting state.
    pub fn new_starting(
        session: SessionName,
        run_id: RunId,
        generation: Generation,
        launch_spec: LaunchSpec,
        sidecar: ProcessIdentity,
        started_at: EpochTimestamp,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            session,
            run_id,
            generation,
            launch_spec,
            sidecar,
            status: RunStatus::Starting,
            started_at,
            restart_count: 0,
            warnings: vec![],
        }
    }

    // --- Accessors ---

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn session(&self) -> &SessionName {
        &self.session
    }

    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Compute canonical hash on demand. Never stale.
    pub fn launch_spec_hash(&self) -> String {
        self.launch_spec.canonical_hash()
    }

    pub fn launch_spec(&self) -> &LaunchSpec {
        &self.launch_spec
    }

    pub fn sidecar(&self) -> &ProcessIdentity {
        &self.sidecar
    }

    pub fn status(&self) -> &RunStatus {
        &self.status
    }

    pub fn started_at(&self) -> &EpochTimestamp {
        &self.started_at
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn add_warning(&mut self, msg: String) {
        self.warnings.push(msg);
    }

    // --- Mutable access for transition module only ---

    pub(super) fn status_mut(&mut self) -> &mut RunStatus {
        &mut self.status
    }
}

impl<'de> Deserialize<'de> for Meta {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            schema_version: u32,
            session: SessionName,
            run_id: RunId,
            generation: Generation,
            launch_spec: LaunchSpec,
            sidecar: ProcessIdentity,
            #[serde(flatten)]
            status: RunStatus,
            started_at: EpochTimestamp,
            restart_count: u32,
            #[serde(default)]
            warnings: Vec<String>,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.schema_version != Meta::SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported schema version: expected {}, got {}",
                Meta::SCHEMA_VERSION,
                raw.schema_version
            )));
        }
        Ok(Meta {
            schema_version: raw.schema_version,
            session: raw.session,
            run_id: raw.run_id,
            generation: raw.generation,
            launch_spec: raw.launch_spec,
            sidecar: raw.sidecar,
            status: raw.status,
            started_at: raw.started_at,
            restart_count: raw.restart_count,
            warnings: raw.warnings,
        })
    }
}
