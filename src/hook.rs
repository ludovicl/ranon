//! Claude Code hook: PostToolUse replaces the tool output with its masked version,
//! PreToolUse puts real values back in what the model writes elsewhere.

use crate::mask::Masker;
use regex::Regex;
use serde_json::{Value, json};
use std::io::Read;
use std::panic::{UnwindSafe, catch_unwind};
use std::sync::{Mutex, OnceLock};

fn psql_tuples_only(command: &str) -> bool {
    static PSQL: OnceLock<Regex> = OnceLock::new();
    static TUPLES: OnceLock<Regex> = OnceLock::new();
    PSQL.get_or_init(|| Regex::new(r"(^|[\s;&|(/])psql(\s|$)").unwrap()).is_match(command)
        && TUPLES
            .get_or_init(|| Regex::new(r"(^|\s)(-[A-Za-z]*t[A-Za-z]*|--tuples-only)(\s|$)").unwrap())
            .is_match(command)
}

/// Files read with the Read tool are masked only when they hold data (exports, logs, API dumps): masking
/// source code would turn its fixtures into tokens that the model then copies into its edits.
const DATA_FILES: &[&str] = &["csv", "tsv", "json", "jsonl", "ndjson", "txt", "log", "xml", "html", "sql"];

fn reads_data_file(payload: &Value) -> bool {
    let path = payload["tool_input"]["file_path"].as_str().unwrap_or_default();
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| DATA_FILES.contains(&ext.to_ascii_lowercase().as_str()))
}

pub(crate) fn walk(value: &mut Value, masker: &mut Masker) -> bool {
    match value {
        // a screenshot cannot be masked, and its base64 would be corrupted by the value patterns
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("image") => false,
        Value::String(text) => {
            let masked = masker.mask_text(text);
            let changed = masked != *text;
            *text = masked;
            changed
        }
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| walk(item, masker) | changed),
        Value::Object(map) => map.values_mut().fold(false, |changed, item| walk(item, masker) | changed),
        _ => false,
    }
}

/// One line per masked or hidden output in `hook.log`: time, tool, outcome, number of values. No content.
pub(crate) fn log(tool_name: &str, outcome: &str, values: usize) {
    write_log(&format!("{tool_name}\t{outcome}\t{values}"));
}

/// A `panic` line in `hook.log`, with where it happened in ranon's code (`src/mask.rs:52`), never its message:
/// `ranon doctor` reports it so that a crash hiding outputs every day does not go unnoticed.
fn log_panic(tool_name: &str, location: &str) {
    write_log(&format!("{tool_name}\tpanic\t0\t{location}"));
}

fn write_log(line: &str) {
    if cfg!(test) {
        // unit tests run without RANON_HOME and must not write to the user's state directory
        return;
    }
    let path = crate::store::state_dir().join("hook.log");
    crate::store::append(&path, &format!("{}\t{line}\n", crate::store::now())).ok();
}

fn masked_response(payload: &Value) -> Result<Option<Value>, String> {
    // before any rule lookup, so the user file is not shadowed by the built-in defaults
    crate::rules::load()?;
    let mut response = payload["tool_response"].clone();
    let tool_name = payload["tool_name"].as_str().unwrap_or_default();
    if tool_name == "Bash" {
        let command = payload["tool_input"]["command"].as_str().unwrap_or_default();
        if !crate::rules::masks_command(command) {
            return Ok(None);
        }
        if psql_tuples_only(command) {
            response["stdout"] =
                json!("[ranon: output hidden, rerun without -t/--tuples-only so columns can be masked]");
            response["stderr"] = json!("");
            log(tool_name, "hidden", 0);
            return Ok(Some(response));
        }
    }
    if tool_name == "Read" && !reads_data_file(payload) {
        return Ok(None);
    }
    let mut masker = Masker::open().map_err(|e| e.to_string())?;
    let before = response.clone();
    masker.mask_json(&mut response);
    if !walk(&mut response, &mut masker) && response == before {
        return Ok(None);
    }
    masker.persist().map_err(|e| e.to_string())?;
    log(tool_name, "masked", masker.store.issued());
    Ok(Some(response))
}

/// PreToolUse on tools that write elsewhere (Notion pages, comments…): puts the real values back in place of
/// tokens, locally, so the model's masked text does not end up in the destination. The call is then shown
/// to the user for approval, since its input now differs from what the model wrote.
fn unmasked_input(payload: &Value) -> Result<Option<(Value, usize)>, String> {
    let masker = Masker::open().map_err(|e| e.to_string())?;
    let mut input = payload["tool_input"].clone();
    let mut restored = 0;
    restore(&mut input, &masker, &mut restored);
    Ok((restored > 0).then_some((input, restored)))
}

pub(crate) fn restore(value: &mut Value, masker: &Masker, restored: &mut usize) {
    match value {
        Value::String(text) => {
            let unmasked = masker.store.unmask(text);
            if unmasked != *text {
                *restored += 1;
                *text = unmasked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| restore(item, masker, restored)),
        Value::Object(map) => map.values_mut().for_each(|item| restore(item, masker, restored)),
        _ => {}
    }
}

fn hidden_output(tool_name: &str, message: String) -> Value {
    if tool_name == "Bash" {
        json!({ "stdout": message, "stderr": "", "interrupted": false, "isImage": false })
    } else if tool_name == "Read" {
        json!({ "type": "text", "file": { "filePath": "", "content": message, "numLines": 1, "startLine": 1, "totalLines": 1 } })
    } else {
        json!([{ "type": "text", "text": message }])
    }
}

fn post_tool_output(updated: Value) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": "PostToolUse", "updatedToolOutput": updated } })
}

/// The tool output replaced by a message, when it cannot be masked.
fn hidden(tool_name: &str, error: &str) -> Value {
    log(tool_name, "hidden", 0);
    hidden_unlogged(tool_name, error)
}

fn hidden_unlogged(tool_name: &str, error: &str) -> Value {
    post_tool_output(hidden_output(tool_name, format!("[ranon: masking failed ({error}), output hidden]")))
}

/// What the hook prints for a payload, if anything.
fn respond(payload: &Value) -> Option<Value> {
    if payload["hook_event_name"] == "PreToolUse" {
        // failing here only means tokens reach the destination, never real data reaching the model
        let (updated, count) = unmasked_input(payload).ok()??;
        let reason = format!("ranon restored {count} masked value(s) before sending: check the real data");
        return Some(json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "ask",
            "permissionDecisionReason": reason, "updatedInput": updated } }));
    }
    match masked_response(payload) {
        Ok(response) => response.map(post_tool_output),
        // fail closed: hide the output rather than leak it
        Err(error) => Some(hidden(payload["tool_name"].as_str().unwrap_or_default(), &error)),
    }
}

struct Panic {
    kind: String,
    location: String,
}

static LAST_PANIC: Mutex<Option<Panic>> = Mutex::new(None);

/// Replaces the default panic hook, which prints the whole panic message to stderr: a message such as
/// "byte index 96 is not a char boundary; it is inside 'é' of `…`" quotes the text being masked.
/// Keeps only the kind of panic and where it happened, for `report_panic`.
pub(crate) fn record_panics() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message =
            payload.downcast_ref::<&str>().copied().or_else(|| payload.downcast_ref::<String>().map(String::as_str));
        let kind = panic_kind(message.unwrap_or_default());
        let location = info.location().map(|at| format!("{}:{}", at.file(), at.line())).unwrap_or_default();
        if let Ok(mut last) = LAST_PANIC.lock() {
            *last = Some(Panic { kind, location });
        }
    }));
}

/// The start of a panic message, cut before anything it may quote (`…`, '…', "…", a value after `:`).
fn panic_kind(message: &str) -> String {
    let kind = message.split([';', ':', '`', '\'', '"', '\n']).next().unwrap_or_default().trim();
    let kind: String = kind.chars().take(80).collect();
    if kind.is_empty() { "panic".to_string() } else { kind }
}

/// After a panic: logs it and returns the error to show in place of the output, its kind and location only.
pub(crate) fn report_panic(tool_name: &str) -> String {
    let panic = LAST_PANIC.lock().ok().and_then(|mut last| last.take());
    log_panic(tool_name, panic.as_ref().map_or("", |panic| panic.location.as_str()));
    match panic {
        Some(Panic { kind, location }) if !location.is_empty() => format!("internal error: {kind} at {location}"),
        Some(Panic { kind, .. }) => format!("internal error: {kind}"),
        None => "internal error".to_string(),
    }
}

/// Runs `respond`, and hides the tool output if it panics: a crashed hook would let the raw output through.
fn fail_closed(payload: &Value, respond: impl FnOnce(&Value) -> Option<Value> + UnwindSafe) -> Option<Value> {
    catch_unwind(|| respond(payload)).unwrap_or_else(|_| {
        let tool_name = payload["tool_name"].as_str().unwrap_or_default();
        let error = report_panic(tool_name);
        let masking = payload["hook_event_name"] != "PreToolUse";
        masking.then(|| hidden_unlogged(tool_name, &error))
    })
}

pub fn claude() -> i32 {
    record_panics();
    let mut raw = Vec::new();
    let read = std::io::stdin().read_to_end(&mut raw);
    let input = String::from_utf8_lossy(&raw);
    let output = match (read, serde_json::from_str::<Value>(&input)) {
        (Ok(_), Ok(payload)) => fail_closed(&payload, respond),
        // unreadable payload: the tool is unknown, guess the shape of its output and hide it
        _ => {
            let tool_name = if input.contains(r#""tool_name":"Bash""#) { "Bash" } else { "" };
            Some(hidden(tool_name, "unreadable hook payload"))
        }
    };
    if let Some(output) = output {
        println!("{output}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_psql_tuples_only() {
        assert!(psql_tuples_only("psql -At -c q"));
        assert!(psql_tuples_only("/usr/bin/psql -t -c q"));
        assert!(!psql_tuples_only("psql --set x=1 -c q"));
        assert!(!psql_tuples_only("ls -t"));
    }

    #[test]
    fn a_panic_hides_the_output_instead_of_leaking_it() {
        let bash = json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash" });
        let output = fail_closed(&bash, |_| panic!("boom")).unwrap();
        let stdout = output["hookSpecificOutput"]["updatedToolOutput"]["stdout"].as_str().unwrap();
        assert!(stdout.contains("output hidden"), "{stdout}");

        let write = json!({ "hook_event_name": "PreToolUse", "tool_name": "mcp__notion__notion-create-pages" });
        assert_eq!(fail_closed(&write, |_| panic!("boom")), None);

        let read = json!({ "hook_event_name": "PostToolUse", "tool_name": "Read" });
        let output = fail_closed(&read, |_| panic!("boom")).unwrap();
        let content = output["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"].as_str().unwrap();
        assert!(content.contains("output hidden"), "{content}");
    }

    #[test]
    fn panic_kind_keeps_no_quoted_text() {
        let slicing = "byte index 96 is not a char boundary; it is inside 'é' (bytes 95..97) of `Jean Dupont é`";
        assert_eq!(panic_kind(slicing), "byte index 96 is not a char boundary");
        let unwrap = "called `Result::unwrap()` on an `Err` value: \"jean@example.com\"";
        assert_eq!(panic_kind(unwrap), "called");
        assert_eq!(panic_kind("index out of bounds: the len is 3 but the index is 5"), "index out of bounds");
        assert_eq!(panic_kind("`Jean Dupont`"), "panic");
    }

    #[test]
    fn only_data_files_are_masked_on_read() {
        let read = |path: &str| json!({ "tool_name": "Read", "tool_input": { "file_path": path } });
        assert!(reads_data_file(&read("/tmp/export.CSV")));
        assert!(reads_data_file(&read("logs/production.log")));
        assert!(!reads_data_file(&read("spec/models/user_spec.rb")));
        assert!(!reads_data_file(&read("Makefile")));
    }
}
