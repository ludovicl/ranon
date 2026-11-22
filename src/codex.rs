//! Codex CLI hook (`ranon hook codex`), a `PostToolUse` hook on MCP tools. Codex lets a hook neither rewrite
//! a tool result (`updatedMCPToolOutput` is parsed but not supported, checked with Codex 0.171.0 on
//! 22/11/2026) nor hide it, but `decision: "block"` replaces the result with the hook's reason. So when a
//! result holds personal data, the hook blocks it and gives its masked version as the reason: the model
//! reads the masked result, reported as a failed call. A result with nothing to mask passes untouched.
//!
//! No `PreToolUse` pass puts real values back in MCP inputs: Codex accepts a rewritten input only with
//! `permissionDecision: "allow"`, which would skip the approval Codex asks for MCP calls.
//!
//! Shell commands are masked by the PATH shims. If ranon is missing, the hook command exits 2, which makes
//! Codex replace the result with an error message.

use crate::hook::{log, report_panic, walk};
use crate::mask::Masker;
use serde_json::{Value, json};
use std::io::Read;
use std::panic::catch_unwind;

fn block(reason: String) -> Value {
    json!({ "decision": "block", "reason": reason })
}

fn hidden(error: &str) -> Value {
    block(format!("[ranon: masking failed ({error}), output hidden]"))
}

fn respond(payload: &Value) -> Result<Value, String> {
    let tool = payload["tool_name"].as_str().unwrap_or_default();
    if payload["hook_event_name"] != "PostToolUse" || !tool.starts_with("mcp__") {
        return Ok(json!({}));
    }
    crate::rules::load()?;
    let mut result = payload["tool_response"].clone();
    let mut masker = Masker::open().map_err(|e| e.to_string())?;
    let before = result.clone();
    masker.mask_json(&mut result);
    if !walk(&mut result, &mut masker) && result == before {
        return Ok(json!({}));
    }
    masker.persist().map_err(|e| e.to_string())?;
    log(tool, "masked", masker.store.issued());
    Ok(block(format!("{NOTE}\n{}", model_text(&result))))
}

/// Codex reports a blocked call as a failure: the note tells the model the call worked and not to retry it.
const NOTE: &str = "[ranon: the call succeeded; personal data in its result is replaced by tokens such as PERSON_1a2b3c. \
Use the result below as is, do not call the tool again.]";

/// The text parts of an MCP result, as the model would have read them; any other shape as JSON.
fn model_text(result: &Value) -> String {
    let texts: Option<Vec<&str>> = result["content"]
        .as_array()
        .filter(|parts| !parts.is_empty())
        .map(|parts| parts.iter().map(|part| part["text"].as_str()).collect::<Option<Vec<_>>>())
        .unwrap_or_default();
    match texts {
        Some(texts) if result.get("structuredContent").is_none() => texts.join("\n"),
        _ => result.to_string(),
    }
}

pub fn run() -> i32 {
    crate::hook::record_panics();
    let mut raw = String::new();
    let payload = std::io::stdin()
        .read_to_string(&mut raw)
        .ok()
        .and_then(|_| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object);
    let Some(payload) = payload else {
        println!("{}", hidden("unreadable hook payload"));
        return 0;
    };
    let tool = payload["tool_name"].as_str().unwrap_or_default().to_string();
    let output = match catch_unwind(|| respond(&payload)) {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            log(&tool, "hidden", 0);
            hidden(&error)
        }
        Err(_) => hidden(&report_panic(&tool)),
    };
    println!("{output}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_the_model_the_text_of_the_result() {
        let text = json!({ "content": [{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }] });
        assert_eq!(model_text(&text), "a\nb");
        let image = json!({ "content": [{ "type": "image", "data": "…" }] });
        assert_eq!(model_text(&image), image.to_string());
        let structured = json!({ "content": [{ "type": "text", "text": "a" }], "structuredContent": { "x": 1 } });
        assert_eq!(model_text(&structured), structured.to_string());
    }
}
