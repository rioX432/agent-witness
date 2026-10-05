use std::io::Write;
use std::process::{Command, Stdio};

use agent_witness_core::{AgentIdentity, AgentName, SessionStore};
use serde_json::{json, Value};

fn command(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-witness"));
    command.env("HOME", home).env_remove("XDG_RUNTIME_DIR");
    command
}

#[test]
fn codex_init_cli_is_isolated_idempotent_and_removes_only_managed_hooks() {
    let home = tempfile::TempDir::new().unwrap();
    let hooks_path = home.path().join(".codex/hooks.json");
    let foreign = json!({"type":"command", "command":"echo foreign", "timeout":7});
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(&hooks_path, serde_json::to_vec(&json!({
        "description":"preserve", "hooks":{"PostToolUse":[{"matcher":"Bash", "hooks":[foreign.clone(), {"type":"command", "command":"agent-witness emit"}]}]}
    })).unwrap()).unwrap();
    let output = command(home.path())
        .args(["init", "--codex"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let first = std::fs::read(&hooks_path).unwrap();
    let settings: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(settings["hooks"]["PostToolUse"][0]["hooks"][0], foreign);
    assert_eq!(
        settings["hooks"]["PostToolUse"][0]["hooks"][1]["command"],
        "agent-witness emit --agent codex"
    );
    let directory_count = std::fs::read_dir(hooks_path.parent().unwrap())
        .unwrap()
        .count();
    assert_eq!(directory_count, 2);
    assert!(command(home.path())
        .args(["init", "--codex"])
        .status()
        .unwrap()
        .success());
    assert_eq!(std::fs::read(&hooks_path).unwrap(), first);
    assert_eq!(
        std::fs::read_dir(hooks_path.parent().unwrap())
            .unwrap()
            .count(),
        directory_count
    );
    assert!(command(home.path())
        .args(["init", "--codex", "--remove"])
        .status()
        .unwrap()
        .success());
    let settings: Value = serde_json::from_slice(&std::fs::read(&hooks_path).unwrap()).unwrap();
    assert_eq!(
        settings,
        json!({"description":"preserve", "hooks":{"PostToolUse":[{"matcher":"Bash", "hooks":[foreign]}]}})
    );
    assert!(!home.path().join(".claude").exists());
    assert!(!home.path().join(".agent-witness").exists());
}

#[test]
fn emit_cli_records_configured_identity_without_changing_raw_stdin() {
    let home = tempfile::TempDir::new().unwrap();
    let raw = "{ \"session_id\":\"cli-codex\", \"hook_event_name\":\"PostToolUse\", \"tool_name\":\"Bash\", \"tool_use_id\":\"toolu_0001\", \"tool_response\":\"synthetic response\" }\n";
    let mut child = command(home.path())
        .args(["emit", "--agent", "codex", "--no-transcript"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(raw.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = SessionStore::new(home.path().join(".agent-witness/sessions"));
    assert_eq!(
        store.read("cli-codex").unwrap().events[0].agent,
        Some(AgentIdentity::configured(AgentName::Codex))
    );
    assert_eq!(store.read_raw("cli-codex").unwrap().records[0].raw, raw);
    assert!(!home.path().join(".claude").exists());
    assert!(!home.path().join(".codex").exists());
}
