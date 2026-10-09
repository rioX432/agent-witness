use std::path::Path;

use agent_witness::digest::{build_digest, to_json, to_markdown, SessionInput, WindowMode};
use agent_witness::report::{
    build_report, JsonExporter, MarkdownExporter, SessionExporter, OBSERVATION_SCOPE_MARKER,
};
use agent_witness::timeline::{build_timeline, ToolStatus};
use agent_witness_core::{
    normalize, AgentEvent, Attribution, EventKind, SessionRead, Source, CONFIDENCE_CERTAIN,
};
use serde_json::{json, Value};

const SCENARIO: &str = "claude-tool-failure";
const BASE_TS: i64 = 1_700_000_000_000;
const TS_STEP: i64 = 1_000;

fn fixture_events() -> Vec<AgentEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(SCENARIO)
        .join("hooks.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let hook: Value = serde_json::from_str(line).unwrap();
            let event =
                normalize(&hook, BASE_TS + i as i64 * TS_STEP, &format!("raw-{i}")).unwrap();
            assert_eq!(event.attribution, Attribution::Direct);
            assert_eq!(event.source, Source::Hooks);
            assert_eq!(event.confidence, CONFIDENCE_CERTAIN);
            assert_eq!(
                event.raw_event_ref.as_deref(),
                Some(format!("raw-{i}").as_str())
            );
            if event.kind == EventKind::ToolFailure {
                for field in [
                    "tool_input",
                    "tool_use_id",
                    "error",
                    "is_interrupt",
                    "duration_ms",
                ] {
                    assert_eq!(event.payload.get(field), hook.get(field));
                }
                assert!(event.payload.get("exit_code").is_none());
            }
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap()
        })
        .collect()
}

#[test]
fn explicit_failures_reach_timeline_report_claim_and_digest() {
    let events = fixture_events();
    let timeline = build_timeline(&events);
    let tools: Vec<_> = timeline
        .iter()
        .filter(|row| row.tool_status.is_some())
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].tool_status, Some(ToolStatus::Failed));
    assert_eq!(tools[0].duration_ms, Some(12));
    assert_eq!(tools[1].tool_status, Some(ToolStatus::Interrupted));
    assert_eq!(tools[1].duration_ms, None);
    assert!(tools.iter().all(|row| row.result.is_some()));

    let read = SessionRead {
        events: events.clone(),
        skipped_lines: 2,
    };
    let report = build_report(SCENARIO, &read, Some(BASE_TS));
    let value: Value = serde_json::from_str(&JsonExporter.export(&report).unwrap()).unwrap();
    for (i, status) in ["failed", "interrupted"].iter().enumerate() {
        assert_eq!(value["commands"][i]["status"], *status);
        assert_eq!(value["commands"][i]["attribution"], "direct");
    }
    assert_eq!(value["timeline"][1]["status"], "failed");
    assert_eq!(value["timeline"][2]["status"], "interrupted");
    assert_eq!(
        value["claim_vs_reality"]["test_commands"][0]["status"],
        "interrupted"
    );
    assert_eq!(value["claim_vs_reality"]["no_result_calls"], 0);
    assert_eq!(value["summary"]["corrupt_lines"], 2);
    let markdown = MarkdownExporter.export(&report).unwrap();
    assert!(markdown.contains("Bash [failed]"));
    assert!(markdown.contains("Bash [interrupted]"));
    assert!(markdown.contains(OBSERVATION_SCOPE_MARKER));

    let digest = build_digest(
        &[SessionInput {
            session_id: SCENARIO.to_string(),
            created_ts: Some(BASE_TS),
            events,
            skipped_lines: 0,
            usage: None,
        }],
        BASE_TS,
        None,
        WindowMode::All,
    );
    let value: Value = serde_json::from_str(&to_json(&digest).unwrap()).unwrap();
    assert_eq!(value["totals"]["test_activity"]["interrupted"], 1);
    assert_eq!(value["projects"][0]["test_activity"]["interrupted"], 1);
    assert_eq!(value["totals"]["test_activity"]["failed"], 0);
    assert!(to_markdown(&digest).contains("1 interrupted"));
}

#[test]
fn orphan_failure_keeps_input_status_and_attribution() {
    for attribution in [
        Attribution::Direct,
        Attribution::Observed,
        Attribution::Inferred,
    ] {
        let events: Vec<_> = fixture_events()
            .into_iter()
            .filter(|event| event.kind == EventKind::ToolFailure)
            .map(|mut event| {
                event.attribution = attribution;
                event
            })
            .collect();
        let timeline = build_timeline(&events);
        assert_eq!(timeline[0].tool_status, Some(ToolStatus::Failed));
        assert_eq!(timeline[1].tool_status, Some(ToolStatus::Interrupted));
        assert_eq!(timeline[0].summary, "cat missing-config.toml");
        assert_eq!(timeline[1].summary, "cargo test");
        assert!(timeline.iter().all(|row| row.attribution == attribution));
    }
}

#[test]
fn only_explicit_true_interrupts_and_error_text_never_sets_outcome() {
    for flag in [
        None,
        Some(json!(false)),
        Some(json!("true")),
        Some(json!(true)),
    ] {
        for paired in [false, true] {
            let mut events = fixture_events();
            let mut failure = events.remove(2);
            failure
                .payload
                .as_object_mut()
                .unwrap()
                .remove("is_interrupt");
            failure.payload["error"] = json!("Exit code 0\ninterrupted");
            if let Some(value) = &flag {
                failure.payload["is_interrupt"] = value.clone();
            }
            let input = if paired {
                vec![events.remove(1), failure]
            } else {
                vec![failure]
            };
            let timeline = build_timeline(&input);
            let expected = if flag == Some(json!(true)) {
                ToolStatus::Interrupted
            } else {
                ToolStatus::Failed
            };
            assert_eq!(timeline[0].tool_status, Some(expected));
        }
    }
}
