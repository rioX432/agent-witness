//! Doc-derived signal fixtures exercise privacy and direct attribution together.
use agent_witness_core::{
    AgentIdentity, AgentName, Attribution, EventKind, FixedClock, RawRecord, Receiver, SessionStore,
};
use serde_json::Value;

const TS: i64 = 1_700_000_000_000;

#[test]
fn signal_fixtures_keep_metadata_and_disclose_source_omissions() {
    let scenarios = [
        (
            "claude-waiting",
            AgentName::ClaudeCode,
            vec![
                EventKind::Notification,
                EventKind::Notification,
                EventKind::Notification,
                EventKind::PermissionRequest,
                EventKind::PermissionDenied,
            ],
        ),
        (
            "codex-waiting",
            AgentName::Codex,
            vec![EventKind::PermissionRequest, EventKind::Interrupt],
        ),
    ];
    for (scenario, agent, kinds) in scenarios {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(scenario);
        let provenance: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("provenance.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["provenance"], "synthetic");
        assert!(provenance["description"]
            .as_str()
            .unwrap()
            .contains("hand-written"));
        assert!(provenance.get("captured_with").is_none());
        assert!(provenance.get("captured_on").is_none());
        let fixture = std::fs::read_to_string(root.join("hooks.jsonl")).unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut receiver = Receiver::new(store.clone());
        for line in fixture.lines() {
            receiver
                .ingest_with_agent(
                    line,
                    &FixedClock(TS),
                    Some(AgentIdentity::configured(agent)),
                )
                .unwrap();
        }
        let read = store.read(scenario).unwrap();
        let raw = store.read_raw(scenario).unwrap();
        assert_eq!(read.skipped_lines, 0);
        assert_eq!(raw.skipped_lines, 0);
        assert_eq!(
            read.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            kinds
        );
        assert_eq!(
            provenance["event_count"].as_u64().unwrap() as usize,
            read.events.len()
        );
        for ((event, record), line) in read.events.iter().zip(&raw.records).zip(fixture.lines()) {
            let hook: Value = serde_json::from_str(line).unwrap();
            assert_eq!(event.attribution, Attribution::Direct);
            assert_eq!(event.agent, Some(AgentIdentity::configured(agent)));
            assert_eq!(
                event.raw_event_ref.as_deref(),
                Some(record.raw_ref.as_str())
            );
            for field in ["notification_type", "tool_name"] {
                assert_eq!(event.payload.get(field), hook.get(field));
            }
            for field in ["message", "title", "reason", "tool_input"] {
                assert!(event.payload.get(field).is_none());
                let retained: Value = serde_json::from_str(&record.raw).unwrap();
                assert!(retained.get(field).is_none());
                assert_eq!(
                    record.omitted_fields.iter().any(|f| f == field),
                    hook.get(field).is_some()
                );
            }
            if event.kind == EventKind::PermissionRequest {
                assert!(event.payload.get("tool_use_id").is_none());
            }
        }
    }
}

#[test]
fn legacy_raw_and_event_records_are_readable() {
    let raw: RawRecord =
        serde_json::from_str(r#"{"v":1,"ts":0,"session":"old","raw_ref":"raw-0","raw":"{}"}"#)
            .unwrap();
    assert!(raw.omitted_fields.is_empty());
    let event: agent_witness_core::AgentEvent = serde_json::from_str(r#"{"v":1,"ts":0,"session":"old","source":"hooks","kind":"tool_call","attribution":"direct","confidence":1.0,"payload":{}}"#).unwrap();
    assert_eq!(event.kind, EventKind::ToolCall);
}

#[test]
fn invalid_session_signal_does_not_leak_text_into_error_source_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = SessionStore::new(tmp.path());
    let mut receiver = Receiver::new(store.clone());
    let result = receiver.ingest(r#"{"session_id":"../bad","hook_event_name":"Notification","notification_type":"idle_prompt","message":"secret text"}"#, &FixedClock(TS)).unwrap();
    assert_eq!(result.kind, EventKind::Error);
    let raw = store.read_raw(&result.session).unwrap();
    assert!(!raw.records[0].raw.contains("secret text"));
    assert_eq!(raw.records[0].omitted_fields, vec!["message"]);
}
