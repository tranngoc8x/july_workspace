use july_workspace::domain::{AgentId, PermissionOutcome, SessionBindingId};
use july_workspace::transport::{
    AcpAgentConfig, AcpTransport, AgentConnection, AgentTransport, CreateSession,
    PermissionResponse, ResumeSession, SessionRef, TransportError, TransportEvent,
    TransportFailureKind,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn config(executable: impl Into<PathBuf>) -> AcpAgentConfig {
    AcpAgentConfig {
        executable: executable.into(),
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        state_directory: std::env::temp_dir(),
        expected_agent_name: "test-acp-agent".into(),
        expected_agent_version: "1.0.0".into(),
    }
}

fn agent() -> AgentConnection {
    AgentConnection {
        agent_id: AgentId::new(),
        project_root: std::env::temp_dir(),
    }
}

fn fixture_config() -> AcpAgentConfig {
    let mut config = config("/usr/bin/python3");
    config.arguments = vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    config
}

#[tokio::test]
async fn connect_rejects_a_relative_executable() {
    let mut transport = AcpTransport::new(config("test-acp-agent"));

    assert!(matches!(
        transport.connect(&agent()).await,
        Err(TransportError::InvalidConfiguration(
            "ACP executable must be an absolute path"
        ))
    ));
}

#[tokio::test]
async fn connect_rejects_a_moving_adapter_argument() {
    let mut moving = config("/usr/bin/python3");
    moving.arguments = vec!["adapter@latest".into()];
    let mut transport = AcpTransport::new(moving);

    assert!(matches!(
        transport.connect(&agent()).await,
        Err(TransportError::InvalidConfiguration(
            "ACP adapter arguments must not use @latest"
        ))
    ));
}

#[tokio::test]
async fn handshake_rejects_wrong_protocol_and_missing_close_capability() {
    let mut wrong_protocol = fixture_config();
    wrong_protocol.arguments.push("--protocol-zero".into());
    let mut transport = AcpTransport::new(wrong_protocol);
    assert!(matches!(
        transport.connect(&agent()).await,
        Err(TransportError::UnsupportedProtocol {
            expected: 1,
            actual: 0
        })
    ));

    let mut no_close = fixture_config();
    no_close.arguments.push("--no-close".into());
    let mut transport = AcpTransport::new(no_close);
    assert!(matches!(
        transport.connect(&agent()).await,
        Err(TransportError::UnsupportedCapability("session/close"))
    ));
}

#[test]
fn event_receiver_can_only_be_subscribed_once() {
    let mut transport = AcpTransport::new(config("/tmp/test-acp-agent"));

    assert!(transport.subscribe().is_ok());
    assert!(matches!(
        transport.subscribe(),
        Err(TransportError::AlreadySubscribed)
    ));
}

#[tokio::test]
async fn subprocess_maps_sessions_permissions_stream_and_missing_resume() {
    let mut transport = AcpTransport::new(fixture_config());
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();

    assert!(matches!(
        transport
            .create_session(CreateSession {
                binding_id: SessionBindingId::new(),
                project_root: PathBuf::from("/"),
            })
            .await,
        Err(TransportError::InvalidConfiguration(
            "session project root must match the connected agent"
        ))
    ));

    let first = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    let second = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    assert_ne!(first.remote_session_id, second.remote_session_id);

    transport
        .send_message(july_workspace::transport::SendMessage {
            session: first.clone(),
            content: "hello".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::TurnStarted {
            session: first.clone()
        })
    );
    let Some(TransportEvent::PermissionRequested(request)) = events.recv().await else {
        panic!("expected permission request");
    };
    transport
        .respond_permission(PermissionResponse {
            session: first.clone(),
            request_id: request.request_id,
            outcome: PermissionOutcome::Selected("allow-once".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::AgentTextDelta {
            session: first.clone(),
            text: "fixture reply".into(),
        })
    );
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::UsageReported {
            session: first.clone(),
            used_tokens: 12,
            context_window_tokens: 4096,
        })
    );
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::AgentMessageCompleted {
            session: first.clone()
        })
    );
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::TurnCompleted {
            session: first.clone()
        })
    );

    let missing = SessionRef {
        binding_id: SessionBindingId::new(),
        remote_session_id: "missing".into(),
    };
    assert!(matches!(
        transport
            .resume_session(ResumeSession {
                session: missing.clone(),
                project_root: std::env::temp_dir(),
            })
            .await,
        Err(TransportError::SessionLost(id)) if id == "missing"
    ));
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::SessionLost { session: missing })
    );
    transport.close_session(first).await.unwrap();
    transport.close_session(second).await.unwrap();
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn two_sessions_interleave_without_losing_per_session_order() {
    let mut transport = AcpTransport::new(fixture_config());
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let first = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    let second = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    for session in [&first, &second] {
        transport
            .send_message(july_workspace::transport::SendMessage {
                session: session.clone(),
                content: "interleave".into(),
            })
            .await
            .unwrap();
    }

    let mut started = BTreeSet::new();
    let mut permissions = Vec::new();
    while started.len() != 2 || permissions.len() != 2 {
        match events.recv().await.unwrap() {
            TransportEvent::TurnStarted { session } => {
                started.insert(session.remote_session_id);
            }
            TransportEvent::PermissionRequested(request) => permissions.push(request),
            event => panic!("unexpected pre-response event: {event:?}"),
        }
    }
    for request in permissions {
        transport
            .respond_permission(PermissionResponse {
                session: request.session,
                request_id: request.request_id,
                outcome: PermissionOutcome::Selected("allow-once".into()),
            })
            .await
            .unwrap();
    }

    let mut order = BTreeMap::<String, Vec<&str>>::new();
    while order.values().filter(|events| events.len() == 4).count() != 2 {
        match events.recv().await.unwrap() {
            TransportEvent::AgentTextDelta { session, .. } => {
                order
                    .entry(session.remote_session_id)
                    .or_default()
                    .push("text");
            }
            TransportEvent::UsageReported { session, .. } => {
                order
                    .entry(session.remote_session_id)
                    .or_default()
                    .push("usage");
            }
            TransportEvent::AgentMessageCompleted { session } => {
                order
                    .entry(session.remote_session_id)
                    .or_default()
                    .push("message");
            }
            TransportEvent::TurnCompleted { session } => {
                order
                    .entry(session.remote_session_id)
                    .or_default()
                    .push("turn");
            }
            event => panic!("unexpected post-response event: {event:?}"),
        }
    }
    assert!(
        order
            .values()
            .all(|events| events == &["text", "usage", "message", "turn"])
    );
    transport.close_session(first).await.unwrap();
    transport.close_session(second).await.unwrap();
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn subprocess_rejects_identity_concurrent_turn_and_unknown_permission() {
    let mut wrong_config = fixture_config();
    wrong_config.expected_agent_version = "wrong".into();
    let mut wrong = AcpTransport::new(wrong_config);
    assert!(matches!(
        wrong.connect(&agent()).await,
        Err(TransportError::UnexpectedAgentIdentity { .. })
    ));

    let mut transport = AcpTransport::new(fixture_config());
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    let message = july_workspace::transport::SendMessage {
        session: session.clone(),
        content: "hello".into(),
    };
    let mut forged = message.clone();
    forged.session.binding_id = SessionBindingId::new();
    assert!(matches!(
        transport.send_message(forged).await,
        Err(TransportError::SessionReferenceMismatch(id))
            if id == session.remote_session_id
    ));
    transport.send_message(message.clone()).await.unwrap();
    assert!(matches!(
        transport.send_message(message).await,
        Err(TransportError::TurnAlreadyActive(id)) if id == session.remote_session_id
    ));
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));
    let Some(TransportEvent::PermissionRequested(request)) = events.recv().await else {
        panic!("expected permission request");
    };
    assert!(matches!(
        transport
            .respond_permission(PermissionResponse {
                session: session.clone(),
                request_id: request.request_id,
                outcome: PermissionOutcome::Selected("unknown".into()),
            })
            .await,
        Err(TransportError::PermissionOptionNotAdvertised(option)) if option == "unknown"
    ));
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "fixture reply".into(),
        })
    );
    while !matches!(
        events.recv().await,
        Some(TransportEvent::TurnCompleted { .. })
    ) {}
    transport.close_session(session).await.unwrap();
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_resolves_permission_and_finishes_the_turn() {
    let mut transport = AcpTransport::new(fixture_config());
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport
        .send_message(july_workspace::transport::SendMessage {
            session: session.clone(),
            content: "cancel me".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::PermissionRequested(_))
    ));
    transport.cancel_turn(session.clone()).await.unwrap();
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "cancelled".into(),
        })
    );
    while !matches!(
        events.recv().await,
        Some(TransportEvent::TurnCompleted { .. })
    ) {}
    transport.close_session(session).await.unwrap();
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_cooperative_cancel_closes_the_agent_connection() {
    let mut config = fixture_config();
    config.arguments.push("--ignore-permission-response".into());
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport
        .send_message(july_workspace::transport::SendMessage {
            session: session.clone(),
            content: "do not stop".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::PermissionRequested(_))
    ));
    transport.cancel_turn(session).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(12), events.recv())
            .await
            .unwrap(),
        Some(TransportEvent::TransportDisconnected { .. })
    ));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn subprocess_eof_emits_one_agent_scoped_disconnect() {
    let mut config = fixture_config();
    config.arguments.push("--exit-after-init".into());
    let expected_agent = agent();
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&expected_agent).await.unwrap();

    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .unwrap(),
        Some(TransportEvent::TransportDisconnected { agent_id, .. })
            if agent_id == expected_agent.agent_id
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), events.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn claude_profile_requires_and_sets_manual_default_mode() {
    let mut missing_mode = fixture_config();
    missing_mode.expected_agent_name = "claude-test".into();
    missing_mode.arguments.push("--claude-no-mode".into());
    let mut transport = AcpTransport::new(missing_mode);
    transport.connect(&agent()).await.unwrap();
    assert!(matches!(
        transport
            .create_session(CreateSession {
                binding_id: SessionBindingId::new(),
                project_root: std::env::temp_dir(),
            })
            .await,
        Err(TransportError::UnsupportedCapability(
            "manual default session mode"
        ))
    ));
    transport.shutdown().await.unwrap();

    let mut manual_mode = fixture_config();
    manual_mode.expected_agent_name = "claude-test".into();
    manual_mode.arguments.push("--claude-mode".into());
    let mut transport = AcpTransport::new(manual_mode);
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport.close_session(session).await.unwrap();
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_and_drains_an_active_turn() {
    let mut config = fixture_config();
    config
        .arguments
        .push("--cancelled-permission-stops-prompt".into());
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport
        .send_message(july_workspace::transport::SendMessage {
            session: session.clone(),
            content: "shutdown me".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::PermissionRequested(_))
    ));

    transport.shutdown().await.unwrap();
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::AgentTextDelta {
            session: session.clone(),
            text: "cancelled".into(),
        })
    );
    while !matches!(
        events.recv().await,
        Some(TransportEvent::TurnCompleted { .. })
    ) {}
}

#[tokio::test]
async fn subprocess_stderr_is_not_exposed_in_events() {
    let mut config = fixture_config();
    config.arguments.push("--secret-error".into());
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let Some(TransportEvent::TransportDisconnected { reason, .. }) =
        tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .unwrap()
    else {
        panic!("expected disconnect");
    };
    assert!(!reason.contains("SECRET_PROVIDER_OUTPUT"));
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn prompt_auth_failure_stays_typed() {
    let mut config = fixture_config();
    config.arguments.push("--auth-error".into());
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport
        .send_message(july_workspace::transport::SendMessage {
            session: session.clone(),
            content: "requires auth".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));
    assert_eq!(
        events.recv().await,
        Some(TransportEvent::TurnFailed {
            session,
            failure: TransportFailureKind::AuthenticationRequired,
        })
    );
    transport.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_waits_for_clean_subprocess_exit() {
    let mut transport = AcpTransport::new(fixture_config());
    transport.connect(&agent()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), transport.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn shutdown_is_bounded_when_a_cancelled_create_never_returns() {
    let mut config = fixture_config();
    config.arguments.push("--hang-new".into());
    let mut transport = AcpTransport::new(config);
    transport.connect(&agent()).await.unwrap();

    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            transport.create_session(CreateSession {
                binding_id: SessionBindingId::new(),
                project_root: std::env::temp_dir(),
            }),
        )
        .await
        .is_err()
    );
    tokio::time::timeout(std::time::Duration::from_secs(11), transport.shutdown())
        .await
        .expect("shutdown must own a deadline independent of the command loop")
        .unwrap();
}

#[tokio::test]
async fn forced_shutdown_responds_cancelled_before_aborting_a_hung_owner() {
    let result_path = std::env::temp_dir().join(format!(
        "july-acp-permission-result-{}-{}",
        std::process::id(),
        AgentId::new()
    ));
    let _ = std::fs::remove_file(&result_path);
    let mut config = fixture_config();
    config.arguments.push("--hang-new-after-first".into());
    config.arguments.push(format!(
        "--permission-result-file={}",
        result_path.display()
    ));
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport
        .send_message(july_workspace::transport::SendMessage {
            session,
            content: "pending before forced shutdown".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::PermissionRequested(_))
    ));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            transport.create_session(CreateSession {
                binding_id: SessionBindingId::new(),
                project_root: std::env::temp_dir(),
            }),
        )
        .await
        .is_err()
    );

    tokio::time::timeout(std::time::Duration::from_secs(11), transport.shutdown())
        .await
        .expect("forced shutdown must retain its independent deadline")
        .unwrap();
    let response = std::fs::read_to_string(&result_path)
        .expect("fixture must receive an explicit permission response before abort");
    assert!(response.contains("cancelled"), "ACP response: {response}");
    std::fs::remove_file(result_path).unwrap();
}

#[tokio::test]
async fn shutdown_cancels_permission_requests_arriving_during_grace() {
    let mut config = fixture_config();
    config.arguments.push("--permission-after-cancel".into());
    let mut transport = AcpTransport::new(config);
    let mut events = transport.subscribe().unwrap();
    transport.connect(&agent()).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;
    transport
        .send_message(july_workspace::transport::SendMessage {
            session,
            content: "permission during shutdown".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        events.recv().await,
        Some(TransportEvent::TurnStarted { .. })
    ));

    tokio::time::timeout(std::time::Duration::from_secs(2), transport.shutdown())
        .await
        .expect("shutdown-time permission must be cancelled immediately")
        .unwrap();
    while let Some(event) = events.try_recv() {
        assert!(!matches!(event, TransportEvent::PermissionRequested(_)));
    }
}

#[tokio::test]
async fn create_rejects_a_duplicate_remote_session_without_replacing_the_first() {
    let mut config = fixture_config();
    config.arguments.push("--duplicate-session-id".into());
    let mut transport = AcpTransport::new(config);
    transport.connect(&agent()).await.unwrap();
    let first = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: std::env::temp_dir(),
        })
        .await
        .unwrap()
        .session;

    assert!(matches!(
        transport
            .create_session(CreateSession {
                binding_id: SessionBindingId::new(),
                project_root: std::env::temp_dir(),
            })
            .await,
        Err(TransportError::SessionReferenceMismatch(remote_id))
            if remote_id == first.remote_session_id
    ));
    transport.close_session(first).await.unwrap();
    transport.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires the pinned Codex ACP adapter installed locally"]
async fn live_codex_initialize_create_close_smoke() {
    run_live_smoke("CODEX", true).await;
}

#[tokio::test]
#[ignore = "requires the pinned Claude ACP adapter installed locally"]
async fn live_claude_initialize_create_close_smoke() {
    run_live_smoke("CLAUDE", false).await;
}

async fn run_live_smoke(profile: &str, no_browser: bool) {
    let value = |suffix: &str| {
        std::env::var(format!("JULY_{profile}_ACP_{suffix}"))
            .unwrap_or_else(|_| panic!("missing JULY_{profile}_ACP_{suffix}"))
    };
    let mut environment = BTreeMap::new();
    if no_browser {
        environment.insert("NO_BROWSER".into(), "1".into());
    }
    let mut transport = AcpTransport::new(AcpAgentConfig {
        executable: PathBuf::from(value("PATH")),
        arguments: Vec::new(),
        environment,
        state_directory: std::env::var_os(format!("JULY_{profile}_ACP_STATE"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir),
        expected_agent_name: value("NAME"),
        expected_agent_version: value("VERSION"),
    });
    let connection = AgentConnection {
        agent_id: AgentId::new(),
        project_root: std::env::current_dir().unwrap(),
    };
    transport.connect(&connection).await.unwrap();
    let session = transport
        .create_session(CreateSession {
            binding_id: SessionBindingId::new(),
            project_root: connection.project_root,
        })
        .await
        .unwrap()
        .session;
    transport.close_session(session).await.unwrap();
    transport.shutdown().await.unwrap();
}
