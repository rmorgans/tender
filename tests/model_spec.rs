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
    let json = r#"{"argv":["echo"],"stdin_mode":"None"}"#;
    let spec: tender::model::spec::LaunchSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.io_mode, tender::model::spec::IoMode::Pipe);
}
