use july_workspace::transport::probe_agent_identity;
use std::path::{Path, PathBuf};

fn fixture() -> Vec<String> {
    vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_agent.py")
            .to_string_lossy()
            .into_owned(),
    ]
}

#[tokio::test]
async fn probe_reads_the_identity_the_adapter_declares() {
    let identity = probe_agent_identity(Path::new("/usr/bin/python3"), &fixture())
        .await
        .expect("probe succeeds");

    assert_eq!(identity.name, "test-acp-agent");
    assert_eq!(identity.version, "1.0.0");
}

#[tokio::test]
async fn probe_reads_the_alternate_identity_of_the_same_fixture() {
    let mut arguments = fixture();
    arguments.push("--claude".into());

    let identity = probe_agent_identity(Path::new("/usr/bin/python3"), &arguments)
        .await
        .expect("probe succeeds");

    assert_eq!(identity.name, "claude-test");
}

#[tokio::test]
async fn probe_rejects_a_relative_executable() {
    let error = probe_agent_identity(Path::new("python3"), &fixture())
        .await
        .expect_err("relative executable is rejected");

    assert!(error.to_string().contains("absolute"), "got {error}");
}

#[tokio::test]
async fn probe_fails_when_the_adapter_speaks_the_wrong_protocol() {
    let mut arguments = fixture();
    arguments.push("--protocol-zero".into());

    assert!(
        probe_agent_identity(Path::new("/usr/bin/python3"), &arguments)
            .await
            .is_err()
    );
}
