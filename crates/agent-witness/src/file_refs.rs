//! Paths referenced by recorded tool inputs, never proof of a filesystem change.

use agent_witness_core::{AgentEvent, EventKind};
use serde_json::Value;

const FIELD_TOOL_INPUT: &str = "tool_input";
const FIELD_FILE_PATH: &str = "file_path";
const FIELD_COMMAND: &str = "command";
const ADD_FILE_HEADER: &str = "*** Add File: ";
const UPDATE_FILE_HEADER: &str = "*** Update File: ";
const DELETE_FILE_HEADER: &str = "*** Delete File: ";
const MOVE_TO_HEADER: &str = "*** Move to: ";

pub(crate) struct FileReference<'a> {
    pub path: &'a str,
    pub tool: String,
}

pub(crate) fn file_references<'a>(tool: &str, call: &'a AgentEvent) -> Vec<FileReference<'a>> {
    if call.kind != EventKind::ToolCall {
        return Vec::new();
    }
    let Some(input) = call.payload.get(FIELD_TOOL_INPUT) else {
        return Vec::new();
    };
    if tool != "apply_patch" {
        return input
            .get(FIELD_FILE_PATH)
            .and_then(Value::as_str)
            .map(|path| FileReference {
                path,
                tool: tool.to_string(),
            })
            .into_iter()
            .collect();
    }
    let mut references = Vec::new();
    match input.get(FIELD_COMMAND) {
        Some(Value::String(patch)) => collect_patch_references(patch, &mut references),
        Some(Value::Array(arguments)) => {
            for argument in arguments.iter().filter_map(Value::as_str) {
                collect_patch_references(argument, &mut references);
            }
        }
        _ => {}
    }
    references
}

fn collect_patch_references<'a>(patch: &'a str, references: &mut Vec<FileReference<'a>>) {
    let mut previous_update = false;
    for line in patch.lines().map(str::trim_end) {
        let header = [
            (ADD_FILE_HEADER, "add"),
            (UPDATE_FILE_HEADER, "update"),
            (DELETE_FILE_HEADER, "delete"),
            (MOVE_TO_HEADER, "move to"),
        ]
        .into_iter()
        .find_map(|(prefix, operation)| {
            line.strip_prefix(prefix)
                .filter(|path| !path.is_empty())
                .map(|path| (path, operation))
        });
        if let Some((path, operation)) = header {
            // Move to follows Update File; keep diff-line prefixes intact to avoid
            // treating patch content as headers: https://github.com/openai/codex/blob/main/codex-rs/apply-patch/src/parser.rs
            if operation == "move to" && previous_update {
                if let Some(source) = references.last_mut() {
                    source.tool = "apply_patch (move from)".to_string();
                }
            }
            references.push(FileReference {
                path,
                tool: format!("apply_patch ({operation})"),
            });
            previous_update = operation == "update";
        } else {
            previous_update = false;
        }
    }
}
