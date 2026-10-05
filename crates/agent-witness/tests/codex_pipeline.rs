use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_witness::digest::{build_digest, to_json, SessionInput, WindowMode};
use agent_witness::report::{build_report, JsonExporter, MarkdownExporter, SessionExporter};
use agent_witness::timeline::{build_timeline, ToolStatus};
use agent_witness::{emit, watch};
use agent_witness_core::{
    normalize, AgentBasis, AgentEvent, AgentIdentity, AgentName, Attribution, Clock, EventKind,
    FixedClock, Receiver, SessionMeta, SessionStore, SCHEMA_VERSION,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

const SCENARIO: &str = "codex-completed";
const FIXED_TS: i64 = 1_700_000_000_000;
const NO_TRANSCRIPT: bool = false;
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn configured(name: AgentName) -> AgentIdentity {
    AgentIdentity {
        name,
        basis: AgentBasis::Configured,
    }
}

fn fixture_lines(scenario: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(scenario)
        .join("hooks.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_codex_store(store: &SessionStore) {
    let read = store.read(SCENARIO).unwrap();
    let lines = fixture_lines(SCENARIO);
    assert_eq!(read.skipped_lines, 0);
    assert_eq!(read.events.len(), lines.len());
    for event in &read.events {
        assert_eq!(event.agent, Some(configured(AgentName::Codex)));
        assert_eq!(event.attribution, Attribution::Direct);
    }
    assert_eq!(
        store.read_meta(SCENARIO).unwrap().agent,
        Some(configured(AgentName::Codex))
    );
    let raw = store.read_raw(SCENARIO).unwrap();
    assert_eq!(raw.skipped_lines, 0);
    assert_eq!(raw.records.len(), lines.len());
    for (record, line) in raw.records.iter().zip(lines) {
        assert_eq!(record.raw, line);
        assert!(read
            .events
            .iter()
            .any(|event| event.raw_event_ref.as_deref() == Some(record.raw_ref.as_str())));
    }
    let timeline = build_timeline(&read.events);
    let statuses: Vec<_> = timeline
        .iter()
        .filter_map(|entry| entry.tool_status)
        .collect();
    assert_eq!(statuses, [ToolStatus::Completed, ToolStatus::Completed]);

    let report = build_report(SCENARIO, &read, Some(FIXED_TS));
    let json: Value = serde_json::from_str(&JsonExporter.export(&report).unwrap()).unwrap();
    assert_eq!(json["agent"]["name"], "codex");
    assert_eq!(json["agent"]["basis"], "configured");
    for command in json["commands"].as_array().unwrap() {
        assert_eq!(command["status"], "completed");
    }
    let tool_rows: Vec<_> = json["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["status"].is_string())
        .collect();
    assert_eq!(tool_rows.len(), 2);
    assert!(tool_rows.iter().all(|entry| entry["status"] == "completed"));
    let claim_commands = json["claim_vs_reality"]["test_commands"]
        .as_array()
        .unwrap();
    assert_eq!(claim_commands.len(), 2);
    assert!(claim_commands
        .iter()
        .all(|entry| entry["status"] == "completed"));
    let markdown = MarkdownExporter.export(&report).unwrap();
    assert!(markdown.contains("completed (exit unknown)"));
    assert!(markdown.contains("codex"));

    let digest = build_digest(
        &[SessionInput {
            session_id: SCENARIO.to_owned(),
            created_ts: Some(FIXED_TS),
            events: read.events,
            skipped_lines: 0,
            usage: None,
        }],
        FIXED_TS,
        None,
        WindowMode::All,
    );
    let digest_json: Value = serde_json::from_str(&to_json(&digest).unwrap()).unwrap();
    let activity = &digest_json["totals"]["test_activity"];
    assert_eq!(activity["test"], 2);
    assert_eq!(activity["completed"], 2);
    assert_eq!(activity["ok"], 0);
    assert_eq!(activity["failed"], 0);
    assert_eq!(activity["no_result"], 0);
    assert_eq!(digest_json["projects"][0]["agent"]["name"], "codex");
}

#[tokio::test]
async fn configured_codex_fallback_preserves_payload_identity_and_honest_status() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let socket = tmp.path().join("absent.sock");
    for line in fixture_lines(SCENARIO) {
        assert_eq!(
            emit::run_emit_with_agent(
                &socket,
                &sessions_root,
                line,
                NO_TRANSCRIPT,
                Some(configured(AgentName::Codex)),
            )
            .await
            .unwrap(),
            emit::EmitOutcome::Fallback
        );
    }
    assert_codex_store(&SessionStore::new(&sessions_root));
}

#[tokio::test]
async fn configured_codex_daemon_preserves_payload_identity_and_honest_status() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let socket = tmp.path().join("witness.sock");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock(FIXED_TS));
    let server = {
        let socket = socket.clone();
        let sessions_root = sessions_root.clone();
        tokio::spawn(async move {
            watch::run_watch(&socket, &sessions_root, clock, NO_TRANSCRIPT, async {
                let _ = shutdown_rx.await;
            })
            .await
        })
    };
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    while !socket.exists() {
        if server.is_finished() {
            panic!("daemon failed before binding: {:?}", server.await.unwrap());
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon socket timeout"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    for line in fixture_lines(SCENARIO) {
        assert_eq!(
            emit::run_emit_with_agent(
                &socket,
                &sessions_root,
                line,
                NO_TRANSCRIPT,
                Some(configured(AgentName::Codex)),
            )
            .await
            .unwrap(),
            emit::EmitOutcome::Forwarded
        );
    }
    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();
    assert_codex_store(&SessionStore::new(&sessions_root));
}

#[test]
fn configured_claude_fixture_retains_success_contract() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut receiver = Receiver::new(SessionStore::new(tmp.path()));
    for line in fixture_lines("session-basic") {
        receiver
            .ingest_with_agent(
                &line,
                &FixedClock(FIXED_TS),
                Some(configured(AgentName::ClaudeCode)),
            )
            .unwrap();
    }
    let read = receiver.store().read("session-basic").unwrap();
    assert!(read
        .events
        .iter()
        .all(|event| event.agent == Some(configured(AgentName::ClaudeCode))));
    assert_eq!(
        receiver.store().read_meta("session-basic").unwrap().agent,
        Some(configured(AgentName::ClaudeCode))
    );
    assert!(build_timeline(&read.events)
        .iter()
        .filter_map(|entry| entry.tool_status)
        .all(|status| status == ToolStatus::Ok));
}

#[test]
fn legacy_codex_path_is_inferred_in_report_but_not_rewritten() {
    let events: Vec<AgentEvent> = fixture_lines(SCENARIO)
        .iter()
        .map(|line| {
            let value: Value = serde_json::from_str(line).unwrap();
            let mut event = normalize(&value, FIXED_TS, "raw-legacy").unwrap();
            event.agent = None;
            event.payload["transcript_path"] = value["transcript_path"].clone();
            event
        })
        .collect();
    assert!(events.iter().all(|event| event.agent.is_none()));
    let read = agent_witness_core::SessionRead {
        events,
        skipped_lines: 0,
    };
    let report = build_report(SCENARIO, &read, Some(FIXED_TS));
    let json: Value = serde_json::from_str(&JsonExporter.export(&report).unwrap()).unwrap();
    assert_eq!(json["agent"]["name"], "codex");
    assert_eq!(json["agent"]["basis"], "inferred");
    assert!(MarkdownExporter
        .export(&report)
        .unwrap()
        .contains("codex (inferred)"));
    assert!(report
        .commands
        .iter()
        .all(|command| command.status == "completed"));
    assert!(read.events.iter().all(|event| event.agent.is_none()));
}

#[test]
fn identityless_result_without_home_hint_never_claims_success() {
    let events: Vec<_> = fixture_lines(SCENARIO)
        .iter()
        .map(|line| {
            let mut value: Value = serde_json::from_str(line).unwrap();
            value["transcript_path"] =
                Value::String("/home/user/project/.codex/session.jsonl".to_owned());
            let mut event = normalize(&value, FIXED_TS, "raw-unknown").unwrap();
            event.agent = None;
            event
        })
        .collect();
    assert!(build_timeline(&events)
        .iter()
        .filter_map(|entry| entry.tool_status)
        .all(|status| status == ToolStatus::Completed));
    let read = agent_witness_core::SessionRead {
        events,
        skipped_lines: 0,
    };
    let json: Value = serde_json::from_str(
        &JsonExporter
            .export(&build_report(SCENARIO, &read, None))
            .unwrap(),
    )
    .unwrap();
    assert!(json["agent"].is_null());
    assert!(json["commands"]
        .as_array()
        .unwrap()
        .iter()
        .all(|command| command["status"] == "completed"));
}

#[test]
fn legacy_raw_path_infers_codex_without_rewriting_any_store_line() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_dir = tmp.path().join(SCENARIO);
    std::fs::create_dir(&session_dir).unwrap();
    let mut events_jsonl = String::new();
    let mut raw_jsonl = String::new();
    for (index, line) in fixture_lines(SCENARIO).iter().enumerate() {
        let reference = format!("raw-{index}");
        let event = normalize(&serde_json::from_str(line).unwrap(), FIXED_TS, &reference).unwrap();
        let mut legacy = serde_json::to_value(event).unwrap();
        legacy.as_object_mut().unwrap().remove("agent");
        events_jsonl.push_str(&format!("{legacy}\n"));
        let raw = serde_json::json!({
            "v": 1, "ts": FIXED_TS, "session": SCENARIO,
            "raw_ref": reference, "raw": line
        });
        raw_jsonl.push_str(&format!("{raw}\n"));
    }
    let events_path = session_dir.join("events.jsonl");
    let raw_path = session_dir.join("raw.jsonl");
    std::fs::write(&events_path, &events_jsonl).unwrap();
    std::fs::write(&raw_path, &raw_jsonl).unwrap();
    let read = SessionStore::new(tmp.path()).read(SCENARIO).unwrap();
    assert_eq!(read.skipped_lines, 0);
    assert!(read.events.iter().all(|event| event.agent
        == Some(AgentIdentity {
            name: AgentName::Codex,
            basis: AgentBasis::Inferred,
        })));
    let report = build_report(SCENARIO, &read, Some(FIXED_TS));
    assert!(report
        .commands
        .iter()
        .all(|command| command.status == "completed"));
    assert!(MarkdownExporter
        .export(&report)
        .unwrap()
        .contains("codex (inferred)"));
    assert_eq!(std::fs::read_to_string(events_path).unwrap(), events_jsonl);
    assert_eq!(std::fs::read_to_string(raw_path).unwrap(), raw_jsonl);
}

#[test]
fn additive_agent_field_keeps_version_one_and_legacy_store_readable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionStore::new(tmp.path());
    let session_dir = tmp.path().join("legacy-session");
    std::fs::create_dir(&session_dir).unwrap();
    let legacy = serde_json::json!({
        "v": 1, "ts": FIXED_TS, "session": "legacy-session", "source": "hooks",
        "kind": "session_start", "attribution": "direct", "confidence": 1.0,
        "correlation_window_ms": null, "raw_event_ref": "raw-legacy", "payload": {}
    });
    let legacy_line = format!("{legacy}\n");
    std::fs::write(session_dir.join("events.jsonl"), &legacy_line).unwrap();
    std::fs::write(
        session_dir.join("meta.json"),
        format!("{{\"v\":1,\"session_id\":\"legacy-session\",\"created_ts\":{FIXED_TS}}}"),
    )
    .unwrap();
    let read = store.read("legacy-session").unwrap();
    assert_eq!(read.skipped_lines, 0);
    assert_eq!(read.events[0].agent, None);
    assert_eq!(store.read_meta("legacy-session").unwrap().agent, None);
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(
        std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap(),
        legacy_line
    );

    #[derive(Deserialize)]
    struct LegacyEventReader {
        v: u32,
        kind: EventKind,
    }
    #[derive(Deserialize)]
    struct LegacyMetaReader {
        v: u32,
        session_id: String,
    }
    let mut new_event = read.events[0].clone();
    new_event.agent = Some(configured(AgentName::Codex));
    let encoded = serde_json::to_string(&new_event).unwrap();
    let old_reader: LegacyEventReader = serde_json::from_str(&encoded).unwrap();
    assert_eq!(old_reader.v, 1);
    assert_eq!(old_reader.kind, EventKind::SessionStart);
    let new_meta = SessionMeta {
        v: SCHEMA_VERSION,
        session_id: "new-session".to_owned(),
        created_ts: FIXED_TS,
        agent: Some(configured(AgentName::Codex)),
    };
    let old_meta: LegacyMetaReader =
        serde_json::from_str(&serde_json::to_string(&new_meta).unwrap()).unwrap();
    assert_eq!(old_meta.v, 1);
    assert_eq!(old_meta.session_id, "new-session");
}
