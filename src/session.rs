use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::model::ids::SessionName;
use crate::model::meta::Meta;

/// Errors from session directory operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session already exists: {0}")]
    AlreadyExists(String),
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("corrupt session {session}: {reason}")]
    Corrupt { session: String, reason: String },
    #[error("session is locked by another process: {0}")]
    Locked(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Root directory for all sessions. Overridable for tests.
#[derive(Debug, Clone)]
pub struct SessionRoot(PathBuf);

impl SessionRoot {
    /// Default: ~/.tender/sessions/
    pub fn default_path() -> anyhow::Result<Self> {
        let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
        Ok(Self(PathBuf::from(home).join(".tender").join("sessions")))
    }

    /// Explicit path (for tests or custom deployments).
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Handle to an existing session directory.
#[derive(Debug)]
pub struct SessionDir {
    path: PathBuf,
    name: SessionName,
}

impl SessionDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &SessionName {
        &self.name
    }

    pub fn meta_path(&self) -> PathBuf {
        self.path.join("meta.json")
    }

    fn meta_tmp_path(&self) -> PathBuf {
        self.path.join("meta.json.tmp")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.path.join("lock")
    }
}

/// Create a new session directory. Fails if it already exists.
pub fn create(root: &SessionRoot, name: &SessionName) -> Result<SessionDir, SessionError> {
    let path = root.path().join(name.as_str());
    if path.exists() {
        return Err(SessionError::AlreadyExists(name.to_string()));
    }
    fs::create_dir_all(&path)?;
    Ok(SessionDir {
        path,
        name: name.clone(),
    })
}

/// Open an existing session directory. Returns None if it doesn't exist.
/// Returns Corrupt if the path exists but is not a directory, or if
/// the directory exists but has no meta.json (newly created dirs
/// that haven't had meta written yet are not valid open targets).
pub fn open(root: &SessionRoot, name: &SessionName) -> Result<Option<SessionDir>, SessionError> {
    let path = root.path().join(name.as_str());
    if !path.exists() {
        return Ok(None);
    }
    if !path.is_dir() {
        return Err(SessionError::Corrupt {
            session: name.to_string(),
            reason: "not a directory".into(),
        });
    }
    let meta_path = path.join("meta.json");
    if !meta_path.exists() {
        return Err(SessionError::Corrupt {
            session: name.to_string(),
            reason: "meta.json not found".into(),
        });
    }
    Ok(Some(SessionDir {
        path,
        name: name.clone(),
    }))
}

/// List all session directory names under root.
/// Returns only directories with valid session names.
/// Non-directory entries and invalid names (hidden files, underscore-prefixed)
/// are silently skipped — these are not sessions.
/// Directories with valid names but missing/corrupt meta.json ARE included —
/// use `open()` or `read_meta()` to distinguish healthy from corrupt sessions.
pub fn list(root: &SessionRoot) -> Result<Vec<SessionName>, SessionError> {
    let root_path = root.path();
    if !root_path.exists() {
        return Ok(vec![]);
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(root_path)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        if let Some(name_str) = entry.file_name().to_str() {
            if let Ok(name) = SessionName::new(name_str) {
                sessions.push(name);
            }
        }
    }
    sessions.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(sessions)
}

/// Read meta.json from a session directory.
/// Returns Corrupt if meta.json is missing or contains invalid JSON.
pub fn read_meta(session: &SessionDir) -> Result<Meta, SessionError> {
    let meta_path = session.meta_path();
    if !meta_path.exists() {
        return Err(SessionError::Corrupt {
            session: session.name().to_string(),
            reason: "meta.json not found".into(),
        });
    }
    let content = fs::read_to_string(&meta_path)?;
    let meta: Meta = serde_json::from_str(&content).map_err(|e| SessionError::Corrupt {
        session: session.name().to_string(),
        reason: format!("invalid meta.json: {e}"),
    })?;
    Ok(meta)
}

/// Write meta.json atomically via temp file + rename.
/// Crash-safe: write tmp → fsync tmp → rename → fsync parent dir.
/// Never leaves partial JSON behind.
pub fn write_meta_atomic(session: &SessionDir, meta: &Meta) -> Result<(), SessionError> {
    let tmp_path = session.meta_tmp_path();
    let meta_path = session.meta_path();

    let json = serde_json::to_string_pretty(meta)?;

    // Write and fsync the temp file
    let mut file = File::create(&tmp_path)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    drop(file);

    // Atomic rename
    fs::rename(&tmp_path, &meta_path)?;

    // Fsync the parent directory to ensure the directory entry update is durable.
    // Without this, the rename could be lost on power failure.
    let dir = File::open(session.path())?;
    dir.sync_all()?;

    Ok(())
}

/// Exclusive file lock on a session. Drop releases the lock.
/// Uses flock — exclusive across processes, not just threads.
#[cfg(unix)]
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
}

#[cfg(unix)]
impl LockGuard {
    /// Try to acquire an exclusive lock. Returns Locked if held by another process.
    pub fn try_acquire(session: &SessionDir) -> Result<Self, SessionError> {
        use std::os::unix::io::AsRawFd;

        let lock_path = session.lock_path();
        let file = File::create(&lock_path)?;

        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                return Err(SessionError::Locked(session.name().to_string()));
            }
            return Err(SessionError::Io(err));
        }

        Ok(Self { _file: file })
    }

    /// Blocking acquire — waits until lock is available.
    pub fn acquire(session: &SessionDir) -> Result<Self, SessionError> {
        use std::os::unix::io::AsRawFd;

        let lock_path = session.lock_path();
        let file = File::create(&lock_path)?;

        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(SessionError::Io(std::io::Error::last_os_error()));
        }

        Ok(Self { _file: file })
    }
}
