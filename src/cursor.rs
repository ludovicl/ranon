//! Cursor hook (`ranon hook cursor`). Cursor cannot rewrite the output of a shell command, only its input,
//! and can replace the output of MCP tools only. So:
//! - `preToolUse` on `Shell`: a command whose output ranon masks is rewritten to run through ranon
//!   (`ranon sh -c '…'`), which masks it before Cursor reads it;
//! - `postToolUse` on MCP tools: the result is masked (`updated_mcp_tool_output`);
//! - `preToolUse` on MCP tools: tokens in the input become real values again. Cursor has no approval
//!   step for a rewritten input, unlike Claude Code.
//!
//! The hooks are installed with `failClosed`, so a crashed hook blocks the call instead of letting it run.

use crate::hook::{log, restore, walk};
use crate::mask::Masker;
use serde_json::{Value, json};
use std::io::Read;
use std::panic::catch_unwind;

const WRAPPER: &str = "ranon sh -c ";

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The shell command, run through ranon when its output is masked.
fn wrapped(command: &str) -> Option<String> {
    (!command.starts_with(WRAPPER) && crate::rules::masks_command(command))
        .then(|| format!("{WRAPPER}{}", quote(command)))
}

fn hidden(error: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("[ranon: masking failed ({error}), output hidden]") }] })
}

/// The MCP result masked, or `None` when it holds nothing to mask.
fn masked_output(output: &str) -> Result<Option<Value>, String> {
    // Cursor passes the result as a JSON string; a result that is not JSON is masked as text
    let mut result: Value =
        serde_json::from_str(output).unwrap_or_else(|_| json!({ "content": [{ "type": "text", "text": output }] }));
    let mut masker = Masker::open().map_err(|e| e.to_string())?;
    let before = result.clone();
    masker.mask_json(&mut result);
    if !walk(&mut result, &mut masker) && result == before {
        return Ok(None);
    }
    masker.persist().map_err(|e| e.to_string())?;
    log("cursor-mcp", "masked", masker.store.issued());
    Ok(Some(result))
}

fn respond(payload: &Value) -> Result<Value, String> {
    crate::rules::load()?;
    let tool = payload["tool_name"].as_str().unwrap_or_default();
    let mut input = payload["tool_input"].clone();
    match payload["hook_event_name"].as_str().unwrap_or_default() {
        "preToolUse" if tool == "Shell" => {
            let command = input["command"].as_str().unwrap_or_default();
            Ok(match wrapped(command) {
                Some(command) => {
                    input["command"] = command.into();
                    json!({ "updated_input": input })
                }
                None => json!({}),
            })
        }
        "preToolUse" => {
            let masker = Masker::open().map_err(|e| e.to_string())?;
            let mut restored = 0;
            restore(&mut input, &masker, &mut restored);
            Ok(if restored > 0 { json!({ "updated_input": input }) } else { json!({}) })
        }
        "postToolUse" if tool != "Shell" => {
            let output = match &payload["tool_output"] {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            Ok(match masked_output(&output) {
                Ok(Some(result)) => json!({ "updated_mcp_tool_output": result }),
                Ok(None) => json!({}),
                Err(error) => {
                    log("cursor-mcp", "hidden", 0);
                    json!({ "updated_mcp_tool_output": hidden(&error) })
                }
            })
        }
        _ => Ok(json!({})),
    }
}

/// Exit 2 makes Cursor block the call: used when the payload or the state cannot be handled.
pub fn run() -> i32 {
    crate::hook::record_panics();
    let mut raw = String::new();
    let payload = std::io::stdin()
        .read_to_string(&mut raw)
        .ok()
        .and_then(|_| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object);
    let Some(payload) = payload else {
        eprintln!("ranon: unreadable hook payload, call blocked");
        return 2;
    };
    let error = match catch_unwind(|| respond(&payload)) {
        Ok(Ok(output)) => {
            println!("{output}");
            return 0;
        }
        Ok(Err(error)) => error,
        Err(_) => crate::hook::internal_error(),
    };
    if payload["hook_event_name"] == "postToolUse" {
        // the tool has already run: hide its output rather than block what can no longer be blocked
        log("cursor-mcp", "hidden", 0);
        println!("{}", json!({ "updated_mcp_tool_output": hidden(&error) }));
        return 0;
    }
    eprintln!("ranon: {error}, call blocked");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_sh() {
        assert_eq!(quote("echo 'a b'"), r"'echo '\''a b'\'''");
    }
}
