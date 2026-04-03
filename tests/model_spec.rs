#[test]
fn launch_spec_io_mode_defaults_to_pipe() {
    let spec = tender::model::spec::LaunchSpec::new(vec!["echo".into()]).unwrap();
    assert_eq!(spec.io_mode, tender::model::spec::IoMode::Pipe);
}

#[test]
fn launch_spec_pty_mode_serializes() {
    let mut spec = tender::model::spec::LaunchSpec::new(vec!["bash".into()]).unwrap();
    spec.io_mode = tender::model::spec::IoMode::Pty;
    let json = serde_json::to_string(&spec).unwrap();
    assert!(json.contains("\"io_mode\":\"Pty\""), "json: {json}");
}

#[test]
fn launch_spec_without_io_mode_deserializes_as_pipe() {
    let json = r#"{"argv":["echo"],"stdin_mode":"None","exec_target":"None"}"#;
    let spec: tender::model::spec::LaunchSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.io_mode, tender::model::spec::IoMode::Pipe);
}

#[test]
fn launch_spec_python_repl_deserializes() {
    let json = r#"{"argv":["python3"],"stdin_mode":"Pipe","exec_target":"PythonRepl"}"#;
    let spec: tender::model::spec::LaunchSpec = serde_json::from_str(json).unwrap();
    assert_eq!(
        spec.exec_target,
        tender::model::spec::ExecTarget::PythonRepl
    );
}

#[test]
fn launch_spec_duckdb_deserializes() {
    let json = r#"{"argv":["duckdb"],"stdin_mode":"Pipe","exec_target":"DuckDb"}"#;
    let spec: tender::model::spec::LaunchSpec = serde_json::from_str(json).unwrap();
    assert_eq!(
        spec.exec_target,
        tender::model::spec::ExecTarget::DuckDb
    );
}

#[test]
fn launch_spec_duckdb_serializes() {
    let mut spec = tender::model::spec::LaunchSpec::new(vec!["duckdb".into()]).unwrap();
    spec.exec_target = tender::model::spec::ExecTarget::DuckDb;
    spec.stdin_mode = tender::model::spec::StdinMode::Pipe;
    let json = serde_json::to_string(&spec).unwrap();
    assert!(json.contains("\"DuckDb\""), "json: {json}");
}
