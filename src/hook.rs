//! Claude Code PostToolUse hook: replaces the tool output with its masked version.

use crate::mask::Masker;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
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

fn masked_response(payload: &Value) -> Result<Option<Value>, String> {
    // before any rule lookup, so the user file is not shadowed by the built-in defaults
    crate::rules::load()?;
    let mut response = payload["tool_response"].clone();
    if payload["tool_name"] == "Bash" {
        let command = payload["tool_input"]["command"].as_str().unwrap_or_default();
        if !crate::rules::masks_command(command) {
            return Ok(None);
        }
        if psql_tuples_only(command) {
            response["stdout"] =
                json!("[ranon: output hidden, rerun without -t/--tuples-only so columns can be masked]");
            response["stderr"] = json!("");
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
    Ok(Some(response))
}

/// PreToolUse on tools that write elsewhere (Notion pages, comments…): puts the real values back in place of
/// tokens, locally, so the model's masked text does not end up in the destination. The call is then shown
/// to the user for approval, since its input now differs from what the model wrote.
fn unmasked_input(payload: &Value) -> Result<Option<(Value, usize)>, String> {
    let masker = Masker::open().map_err(|e| e.to_string())?;
    let mapping = masker.store.mapping().map_err(|e| e.to_string())?;
    let mut input = payload["tool_input"].clone();
    let mut restored = 0;
    restore(&mut input, &masker, &mapping, &mut restored);
    Ok((restored > 0).then_some((input, restored)))
}

fn restore(value: &mut Value, masker: &Masker, mapping: &HashMap<String, String>, restored: &mut usize) {
    match value {
        Value::String(text) => {
            let unmasked = masker.store.unmask(text, mapping);
            if unmasked != *text {
                *restored += 1;
                *text = unmasked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| restore(item, masker, mapping, restored)),
        Value::Object(map) => map.values_mut().for_each(|item| restore(item, masker, mapping, restored)),
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

pub fn claude() -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&input) else { return 0 };
    if payload["hook_event_name"] == "PreToolUse" {
        // failing here only means tokens reach the destination, never real data reaching the model
        if let Ok(Some((updated, count))) = unmasked_input(&payload) {
            let reason = format!("ranon restored {count} masked value(s) before sending: check the real data");
            println!(
                "{}",
                json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "ask",
                    "permissionDecisionReason": reason, "updatedInput": updated } })
            );
        }
        return 0;
    }
    let tool_name = payload["tool_name"].as_str().unwrap_or_default();
    let updated = match masked_response(&payload) {
        Ok(Some(response)) => response,
        Ok(None) => return 0,
        // fail closed: hide the output rather than leak it
        Err(error) => hidden_output(tool_name, format!("[ranon: masking failed ({error}), output hidden]")),
    };
    println!("{}", json!({ "hookSpecificOutput": { "hookEventName": "PostToolUse", "updatedToolOutput": updated } }));
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
}
