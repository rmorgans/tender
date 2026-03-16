use std::num::NonZeroU32;
use tender::model::ids::{Generation, ProcessIdentity, RunId, SessionName, SessionNameError};

#[test]
fn run_id_uniqueness() {
    let a = RunId::new();
    let b = RunId::new();
    assert_ne!(a, b);
}

#[test]
fn run_id_display_is_uuid_format() {
    let id = RunId::new();
    let s = id.to_string();
    assert_eq!(s.len(), 36);
    assert_eq!(s.chars().filter(|c| *c == '-').count(), 4);
}

#[test]
fn run_id_serde_roundtrip() {
    let id = RunId::new();
    let json = serde_json::to_string(&id).unwrap();
    let back: RunId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn run_id_rejects_non_v7_uuid() {
    // Nil UUID (v0) should be rejected
    let nil = uuid::Uuid::nil();
    let json = serde_json::to_string(&nil).unwrap();
    let result: Result<RunId, _> = serde_json::from_str(&json);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("v7"));
}

#[test]
fn generation_starts_at_one() {
    let g = Generation::first();
    assert_eq!(g.as_u64(), 1);
}

#[test]
fn generation_increments() {
    let g = Generation::first().next().next();
    assert_eq!(g.as_u64(), 3);
}

#[test]
fn generation_serde_roundtrip() {
    let g = Generation::first().next();
    let json = serde_json::to_string(&g).unwrap();
    let back: Generation = serde_json::from_str(&json).unwrap();
    assert_eq!(g, back);
}

#[test]
fn generation_rejects_zero() {
    let result: Result<Generation, _> = serde_json::from_str("0");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("zero"));
}

#[test]
fn session_name_valid() {
    assert!(SessionName::new("upload").is_ok());
    assert!(SessionName::new("my-job").is_ok());
    assert!(SessionName::new("job_123").is_ok());
    assert!(SessionName::new("a").is_ok());
}

#[test]
fn session_name_empty_rejected() {
    assert!(matches!(
        SessionName::new("").unwrap_err(),
        SessionNameError::Empty
    ));
}

#[test]
fn session_name_slash_rejected() {
    assert!(matches!(
        SessionName::new("a/b").unwrap_err(),
        SessionNameError::ContainsSlash
    ));
}

#[test]
fn session_name_dot_rejected() {
    assert!(matches!(
        SessionName::new("a.b").unwrap_err(),
        SessionNameError::ContainsDot
    ));
}

#[test]
fn session_name_whitespace_rejected() {
    assert!(matches!(
        SessionName::new("a b").unwrap_err(),
        SessionNameError::ContainsWhitespace
    ));
    assert!(matches!(
        SessionName::new("a\tb").unwrap_err(),
        SessionNameError::ContainsWhitespace
    ));
}

#[test]
fn session_name_underscore_prefix_rejected() {
    assert!(matches!(
        SessionName::new("_sidecar").unwrap_err(),
        SessionNameError::StartsWithUnderscore
    ));
}

#[test]
fn session_name_serde_roundtrip() {
    let name = SessionName::new("upload").unwrap();
    let json = serde_json::to_string(&name).unwrap();
    let back: SessionName = serde_json::from_str(&json).unwrap();
    assert_eq!(name, back);
}

#[test]
fn session_name_rejects_invalid_on_deserialize() {
    // Empty
    let result: Result<SessionName, _> = serde_json::from_str(r#""""#);
    assert!(result.is_err());

    // Contains slash
    let result: Result<SessionName, _> = serde_json::from_str(r#""a/b""#);
    assert!(result.is_err());

    // Starts with underscore
    let result: Result<SessionName, _> = serde_json::from_str(r#""_bad""#);
    assert!(result.is_err());
}

#[test]
fn process_identity_serde_roundtrip() {
    let id = ProcessIdentity {
        pid: NonZeroU32::new(1234).unwrap(),
        start_time_ns: 1_710_612_345_000_000_000,
    };
    let json = serde_json::to_string(&id).unwrap();
    let back: ProcessIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}
