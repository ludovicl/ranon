//! Claude Code hook: PostToolUse replaces the tool output with its masked version,
//! PreToolUse puts real values back in what the model writes elsewhere.

use crate::mask::Masker;
use regex::Regex;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::panic::{UnwindSafe, catch_unwind};
use std::sync::OnceLock;

fn psql_tuples_only(command: &str) -> bool {
    static PSQL: OnceLock<Regex> = OnceLock::new();
    static TUPLES: OnceLock<Regex> = OnceLock::new();
    PSQL.get_or_init(|| Regex::new(r"(^|[\s;&|(/])psql(\s|$)").unwrap()).is_match(command)
        && TUPLES
            .get_or_init(|| Regex::new(r"(^|\s)(-[A-Za-z]*t[A-Za-z]*|--tuples-only)(\s|$)").unwrap())
            .is_match(command)
}

fn walk(value: &mut Value, masker: &mut Masker) -> bool {
    match value {
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
fn log(tool_name: &str, outcome: &str, values: usize) {
    if cfg!(test) {
        // unit tests run without RANON_HOME and must not write to the user's state directory
        return;
    }
    let path = crate::store::state_dir().join("hook.log");
    if let Ok(mut file) = crate::store::private_file(&path, true) {
        writeln!(file, "{}\t{tool_name}\t{outcome}\t{values}", crate::store::now()).ok();
    }
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

fn restore(value: &mut Value, masker: &Masker, restored: &mut usize) {
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

/// Runs `respond`, and hides the tool output if it panics: a crashed hook would let the raw output through.
fn fail_closed(payload: &Value, respond: impl FnOnce(&Value) -> Option<Value> + UnwindSafe) -> Option<Value> {
    catch_unwind(|| respond(payload)).unwrap_or_else(|_| {
        let masking = payload["hook_event_name"] != "PreToolUse";
        masking.then(|| hidden(payload["tool_name"].as_str().unwrap_or_default(), "internal error"))
    })
}

pub fn claude() -> i32 {
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
    }
}
