//! Per-message model + token usage aggregation over a session transcript.
//!
//! Groundwork for a future cross-session `digest`: it walks every `assistant`
//! line and sums each API response's token usage per model, so a digest can read
//! a cheap [`SessionUsage`] sidecar instead of re-parsing transcripts.
//!
//! This is deliberately SEPARATE from [`super::transcript_v1::parse_line`] (which
//! extracts prose events and intentionally skips tool-only turns): a message can
//! be tool-only yet still carry `usage`, so usage must scan ALL assistant lines.
//!
//! **The `message.id` dedup landmine.** One assistant API response is split
//! across several JSONL lines — one per content block (text, then each
//! `tool_use`) — and EVERY such line repeats the FULL identical `message.usage`.
//! Naive per-line summation multi-counts. We dedup by `message.id` and count each
//! unique message's usage exactly once.
//!
//! Honest observation (Core Value 1): the token totals never claim more than was
//! read. Coverage counters ([`SessionUsage::messages_missing_usage`] etc.) and
//! [`SessionUsage::source_status`] make "no usage found" distinguishable from
//! "0 tokens used", and every record is [`Attribution::Observed`], never
//! `Direct` (the transcript is a secondary source, ADR-0002).

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::event::{Attribution, SCHEMA_VERSION};

use super::ADAPTER_VERSION;

/// Transcript line `type` that carries an assistant API response.
const TYPE_ASSISTANT: &str = "assistant";
/// `source` recorded on a usage record: the transcript adapter produced it.
const TRANSCRIPT_SOURCE: &str = "transcript";

// Field names read out of a transcript line / message object.
const FIELD_TYPE: &str = "type";
const FIELD_MESSAGE: &str = "message";
const FIELD_ID: &str = "id";
const FIELD_MODEL: &str = "model";
const FIELD_USAGE: &str = "usage";
const FIELD_INPUT_TOKENS: &str = "input_tokens";
const FIELD_OUTPUT_TOKENS: &str = "output_tokens";
const FIELD_CACHE_CREATION_INPUT_TOKENS: &str = "cache_creation_input_tokens";
const FIELD_CACHE_READ_INPUT_TOKENS: &str = "cache_read_input_tokens";

/// Whether a transcript read yielded any usable per-message usage.
///
/// Persisted so a reader can tell an honest negative ("read, nothing usable")
/// apart from real zero totals — the absence of the whole file means "not read /
/// no claim", which is a third, distinct state (see [`crate::store`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSourceStatus {
    /// At least one unique assistant message contributed usage.
    Ok,
    /// The transcript was read but no assistant message yielded usable usage.
    NoUsageParsed,
}

/// Token totals for one model, summed over its unique assistant messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// The `message.model` these totals were reported under.
    pub model: String,
    /// Unique assistant messages (deduped by `message.id`) counted for this model.
    pub messages: usize,
    /// Sum of `usage.input_tokens`.
    pub input_tokens: u64,
    /// Sum of `usage.output_tokens`.
    pub output_tokens: u64,
    /// Sum of `usage.cache_creation_input_tokens`.
    pub cache_creation_input_tokens: u64,
    /// Sum of `usage.cache_read_input_tokens`.
    pub cache_read_input_tokens: u64,
}

/// A session's aggregated model + token usage, persisted as the `usage.json`
/// sidecar. Recomputed from the whole transcript and overwritten on each ingest,
/// so it is idempotent by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    /// Schema version this record was written under.
    pub v: u32,
    /// Session id these totals belong to.
    pub session: String,
    /// Adapter that produced the record (always `"transcript"` in v0.1).
    pub source: String,
    /// Whether any usable usage was found (honesty discriminator).
    pub source_status: UsageSourceStatus,
    /// Attribution strength — always [`Attribution::Observed`] (ADR-0002).
    pub attribution: Attribution,
    /// Provenance only: the transcript parser contract version in force. NOT a
    /// schema version for this record (that is [`SessionUsage::v`]); recorded so a
    /// reader knows which line-shape parser produced the totals.
    pub adapter_version: u32,
    /// Unique `message.id`s that carried parseable usage.
    pub messages_counted: usize,
    /// Unique `message.id`s seen but whose usage (or model) was absent/malformed.
    pub messages_missing_usage: usize,
    /// Assistant lines with no `message.id` — cannot attribute, never per-line-summed.
    pub assistant_lines_missing_message_id: usize,
    /// Assistant lines dropped because their `message.id` was already counted.
    pub duplicate_usage_lines_deduped: usize,
    /// Per-model totals, sorted by model name for deterministic output.
    pub per_model: Vec<ModelUsage>,
}

/// The four top-level token counters read from a `usage` object.
#[derive(Debug, Clone, Copy, Default)]
struct TokenCounts {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

/// Outcome of reading one optional `u64` token field.
enum FieldRead {
    /// Field absent — defaults to 0, does not by itself make usage "present".
    Absent,
    /// Field present and a valid non-negative integer.
    Present(u64),
    /// Field present but not a non-negative integer — the usage is malformed.
    Malformed,
}

/// Aggregate per-model token usage from a transcript's contents.
///
/// Pure: does no I/O and reads no wall-clock/RNG. `session` is echoed into the
/// record. Dedups by `message.id`, sums the four top-level `usage.*` token fields
/// per model (NOT `usage.iterations`), and reports honest coverage counters. A
/// line that is not valid JSON, not an object, or not an `assistant` line is
/// ignored here (the prose parser accounts for line-shape skips separately).
pub fn aggregate_transcript_usage(content: &str, session: &str) -> SessionUsage {
    let mut seen_message_ids: HashSet<String> = HashSet::new();
    // BTreeMap keeps per_model deterministically sorted by model name.
    let mut per_model: BTreeMap<String, ModelUsage> = BTreeMap::new();
    let mut messages_counted = 0usize;
    let mut messages_missing_usage = 0usize;
    let mut assistant_lines_missing_message_id = 0usize;
    let mut duplicate_usage_lines_deduped = 0usize;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if obj.get(FIELD_TYPE).and_then(Value::as_str) != Some(TYPE_ASSISTANT) {
            continue;
        }
        // An assistant line with no `message` object has no `message.id` to
        // attribute or dedup by, so it is treated the same as a missing id.
        let Some(message) = obj.get(FIELD_MESSAGE).and_then(Value::as_object) else {
            assistant_lines_missing_message_id += 1;
            continue;
        };
        let message_id = match message.get(FIELD_ID).and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id,
            // No id: we cannot dedup, and summing it would risk multi-counting
            // the same response — so we count it, never sum it.
            _ => {
                assistant_lines_missing_message_id += 1;
                continue;
            }
        };
        if !seen_message_ids.insert(message_id.to_string()) {
            // Same response, another content-block line: identical usage already
            // counted for this id — drop it so we never multi-count.
            duplicate_usage_lines_deduped += 1;
            continue;
        }

        // First (and only counted) sight of this unique message. Both a model to
        // attribute to and parseable usage are required; without a model we would
        // have to fabricate a per-model bucket, which we refuse to do.
        let model = message
            .get(FIELD_MODEL)
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty());
        let counts = message.get(FIELD_USAGE).and_then(read_token_counts);
        match (model, counts) {
            (Some(model), Some(counts)) => {
                messages_counted += 1;
                let entry = per_model
                    .entry(model.to_string())
                    .or_insert_with(|| ModelUsage {
                        model: model.to_string(),
                        messages: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    });
                entry.messages += 1;
                entry.input_tokens = entry.input_tokens.saturating_add(counts.input_tokens);
                entry.output_tokens = entry.output_tokens.saturating_add(counts.output_tokens);
                entry.cache_creation_input_tokens = entry
                    .cache_creation_input_tokens
                    .saturating_add(counts.cache_creation_input_tokens);
                entry.cache_read_input_tokens = entry
                    .cache_read_input_tokens
                    .saturating_add(counts.cache_read_input_tokens);
            }
            _ => messages_missing_usage += 1,
        }
    }

    let source_status = if messages_counted > 0 {
        UsageSourceStatus::Ok
    } else {
        UsageSourceStatus::NoUsageParsed
    };

    SessionUsage {
        v: SCHEMA_VERSION,
        session: session.to_string(),
        source: TRANSCRIPT_SOURCE.to_string(),
        source_status,
        attribution: Attribution::Observed,
        adapter_version: ADAPTER_VERSION,
        messages_counted,
        messages_missing_usage,
        assistant_lines_missing_message_id,
        duplicate_usage_lines_deduped,
        per_model: per_model.into_values().collect(),
    }
}

/// Read the four top-level token fields from a `usage` object.
///
/// Returns `None` when the usage is unusable: not an object, no recognized token
/// field present (e.g. `{}` or a usage carrying only `iterations`), or a present
/// field that is not a non-negative integer (malformed). Absent individual fields
/// default to 0, since older transcript shapes may omit the cache counters.
fn read_token_counts(usage: &Value) -> Option<TokenCounts> {
    let obj = usage.as_object()?;
    let mut counts = TokenCounts::default();
    let mut any_present = false;
    for (field, slot) in [
        (FIELD_INPUT_TOKENS, &mut counts.input_tokens),
        (FIELD_OUTPUT_TOKENS, &mut counts.output_tokens),
        (
            FIELD_CACHE_CREATION_INPUT_TOKENS,
            &mut counts.cache_creation_input_tokens,
        ),
        (
            FIELD_CACHE_READ_INPUT_TOKENS,
            &mut counts.cache_read_input_tokens,
        ),
    ] {
        match read_optional_u64(obj, field) {
            FieldRead::Absent => {}
            FieldRead::Present(n) => {
                *slot = n;
                any_present = true;
            }
            // A present-but-non-numeric field means we cannot trust the totals;
            // treat the whole message's usage as missing rather than under-count.
            FieldRead::Malformed => return None,
        }
    }
    any_present.then_some(counts)
}

/// Classify one optional `u64` field: absent, a valid non-negative integer, or
/// present-but-malformed.
fn read_optional_u64(obj: &Map<String, Value>, field: &str) -> FieldRead {
    match obj.get(field) {
        None => FieldRead::Absent,
        Some(v) => match v.as_u64() {
            Some(n) => FieldRead::Present(n),
            None => FieldRead::Malformed,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One assistant line with a text block, id `msg`, model `m`, and usage.
    fn text_line(id: &str, model: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","content":[{{"type":"text","text":"hi"}}],"usage":{usage}}}}}"#
        )
    }

    const FULL_USAGE: &str = r#"{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":30,"cache_read_input_tokens":40,"iterations":[{"input_tokens":999,"output_tokens":999}]}"#;

    #[test]
    fn empty_transcript_reports_no_usage_parsed() {
        let usage = aggregate_transcript_usage("", "s");
        assert_eq!(usage.source_status, UsageSourceStatus::NoUsageParsed);
        assert_eq!(usage.messages_counted, 0);
        assert!(usage.per_model.is_empty());
        assert_eq!(usage.attribution, Attribution::Observed);
        assert_eq!(usage.source, "transcript");
        assert_eq!(usage.adapter_version, ADAPTER_VERSION);
    }

    #[test]
    fn transcript_with_only_non_assistant_lines_reports_no_usage_parsed() {
        let content = concat!(
            r#"{"type":"user","message":{"content":"hi"}}"#,
            "\n",
            r#"{"type":"queue-operation","operation":"enqueue"}"#,
        );
        let usage = aggregate_transcript_usage(content, "s");
        assert_eq!(usage.source_status, UsageSourceStatus::NoUsageParsed);
        assert_eq!(usage.messages_counted, 0);
        assert_eq!(usage.assistant_lines_missing_message_id, 0);
    }

    #[test]
    fn single_message_split_across_lines_is_counted_once() {
        // One response (msg_0001) split across 3 content-block lines, each
        // repeating the identical usage — the dedup landmine.
        let line = text_line("msg_0001", "claude-fable-5", FULL_USAGE);
        let content = format!("{line}\n{line}\n{line}");

        let usage = aggregate_transcript_usage(&content, "s");

        assert_eq!(usage.messages_counted, 1);
        assert_eq!(usage.duplicate_usage_lines_deduped, 2);
        assert_eq!(usage.per_model.len(), 1);
        let m = &usage.per_model[0];
        assert_eq!(m.model, "claude-fable-5");
        assert_eq!(m.messages, 1);
        // Counted exactly once, not tripled.
        assert_eq!(m.input_tokens, 10);
        assert_eq!(m.output_tokens, 20);
        assert_eq!(m.cache_creation_input_tokens, 30);
        assert_eq!(m.cache_read_input_tokens, 40);
    }

    #[test]
    fn top_level_usage_is_summed_not_iterations() {
        // FULL_USAGE has iterations with 999s; only the top-level 10/20 count.
        let usage = aggregate_transcript_usage(&text_line("a", "m", FULL_USAGE), "s");
        assert_eq!(usage.per_model[0].input_tokens, 10);
        assert_eq!(usage.per_model[0].output_tokens, 20);
    }

    #[test]
    fn multiple_models_are_summed_separately_and_sorted() {
        let content = format!(
            "{}\n{}\n{}",
            text_line(
                "m1",
                "zeta-model",
                r#"{"input_tokens":1,"output_tokens":2}"#
            ),
            text_line(
                "m2",
                "alpha-model",
                r#"{"input_tokens":3,"output_tokens":4}"#
            ),
            text_line(
                "m3",
                "alpha-model",
                r#"{"input_tokens":5,"output_tokens":6}"#
            ),
        );

        let usage = aggregate_transcript_usage(&content, "s");

        assert_eq!(usage.messages_counted, 3);
        // Sorted by model name: alpha before zeta.
        assert_eq!(usage.per_model.len(), 2);
        assert_eq!(usage.per_model[0].model, "alpha-model");
        assert_eq!(usage.per_model[0].messages, 2);
        assert_eq!(usage.per_model[0].input_tokens, 8);
        assert_eq!(usage.per_model[0].output_tokens, 10);
        assert_eq!(usage.per_model[1].model, "zeta-model");
        assert_eq!(usage.per_model[1].messages, 1);
        assert_eq!(usage.per_model[1].input_tokens, 1);
    }

    #[test]
    fn assistant_message_without_usage_counts_as_missing() {
        let line = r#"{"type":"assistant","message":{"id":"a","model":"m","content":[{"type":"text","text":"hi"}]}}"#;
        let usage = aggregate_transcript_usage(line, "s");
        assert_eq!(usage.messages_counted, 0);
        assert_eq!(usage.messages_missing_usage, 1);
        assert_eq!(usage.source_status, UsageSourceStatus::NoUsageParsed);
        assert!(usage.per_model.is_empty());
    }

    #[test]
    fn malformed_non_numeric_token_field_counts_as_missing() {
        let line = text_line("a", "m", r#"{"input_tokens":"lots","output_tokens":20}"#);
        let usage = aggregate_transcript_usage(&line, "s");
        assert_eq!(usage.messages_counted, 0);
        assert_eq!(usage.messages_missing_usage, 1);
        assert!(usage.per_model.is_empty());
    }

    #[test]
    fn empty_usage_object_counts_as_missing() {
        let line = text_line("a", "m", r#"{}"#);
        let usage = aggregate_transcript_usage(&line, "s");
        assert_eq!(usage.messages_missing_usage, 1);
        assert_eq!(usage.messages_counted, 0);
    }

    #[test]
    fn assistant_line_without_message_id_is_not_summed() {
        let line = r#"{"type":"assistant","message":{"model":"m","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let usage = aggregate_transcript_usage(line, "s");
        assert_eq!(usage.assistant_lines_missing_message_id, 1);
        assert_eq!(usage.messages_counted, 0);
        assert_eq!(usage.messages_missing_usage, 0);
        assert!(usage.per_model.is_empty());
    }

    #[test]
    fn assistant_line_without_message_object_counts_as_missing_id() {
        let line = r#"{"type":"assistant","uuid":"x"}"#;
        let usage = aggregate_transcript_usage(line, "s");
        assert_eq!(usage.assistant_lines_missing_message_id, 1);
        assert_eq!(usage.messages_counted, 0);
    }

    #[test]
    fn usage_without_model_counts_as_missing_not_fabricated() {
        // Usage present but no model to attribute it to: we refuse to invent a
        // per-model bucket, so it lands in messages_missing_usage.
        let line = r#"{"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let usage = aggregate_transcript_usage(line, "s");
        assert_eq!(usage.messages_missing_usage, 1);
        assert_eq!(usage.messages_counted, 0);
        assert!(usage.per_model.is_empty());
    }

    #[test]
    fn unparseable_lines_are_ignored_for_usage() {
        let content = concat!(
            "{ broken json",
            "\n",
            r#"{"type":"assistant","message":{"id":"a","model":"m","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#,
        );
        let usage = aggregate_transcript_usage(content, "s");
        // The broken line does not error; the good message is still counted.
        assert_eq!(usage.messages_counted, 1);
        assert_eq!(usage.per_model[0].input_tokens, 10);
    }

    #[test]
    fn record_round_trips_through_json() {
        let usage = aggregate_transcript_usage(&text_line("a", "m", FULL_USAGE), "sess");
        let line = serde_json::to_string(&usage).expect("serialize");
        let back: SessionUsage = serde_json::from_str(&line).expect("deserialize");
        assert_eq!(usage, back);
        // Enum + attribution serialize as snake_case for a stable wire format.
        assert!(line.contains("\"source_status\":\"ok\""));
        assert!(line.contains("\"attribution\":\"observed\""));
    }
}
