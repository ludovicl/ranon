//! OpenCode hook (`ranon hook opencode`), called by the plugin `ranon init --agent opencode` writes. OpenCode
//! hands every tool call to plugins before and after it runs, MCP tools included, and lets them change the
//! input and the result. The plugin sends the event here as JSON and applies the answer:
//! - `after`: the result is masked before the model reads it. Shell commands are left to the PATH shims;
//!   the Read tool is masked on data files only, like in Claude Code; MCP tools and Code Mode (`execute`,
//!   which composes MCP results) are always masked. The result holds the output twice, as `content` parts
//!   and as a structured `output` that Code Mode reuses: both are masked.
//! - `before`: tokens in the input of an MCP tool become real values again, so a write or a search reaches
//!   the real data. OpenCode has no approval step for a rewritten input, unlike Claude Code.
//!
//! Failing after the tool ran hides its result; failing before leaves the tokens in the input.

use crate::hook::{log, report_panic, restore, walk};
use crate::mask::Masker;
use serde_json::{Value, json};
use std::io::Read;
use std::panic::catch_unwind;

/// OpenCode's own tools: anything else comes from an MCP server or a plugin.
pub(crate) const BUILT_IN: &[&str] = &[
    "bash",
    "read",
    "write",
    "edit",
    "multiedit",
    "patch",
    "apply_patch",
    "glob",
    "grep",
    "list",
    "ls",
    "lsp",
    "webfetch",
    "websearch",
    "codesearch",
    "todowrite",
    "todoread",
    "task",
    "skill",
    "question",
    "batch",
    "invalid",
    "plan_enter",
    "plan_exit",
];

/// Code Mode: runs JavaScript that calls MCP tools and returns what they gave.
const CODE_MODE: &str = "execute";

fn is_mcp(tool: &str) -> bool {
    tool != CODE_MODE && !BUILT_IN.contains(&tool)
}

fn reads_data_file(input: &Value) -> bool {
    let path = input["filePath"].as_str().unwrap_or_default();
    crate::hook::is_data_file(path)
}

fn masks_result(tool: &str, input: &Value) -> bool {
    match tool {
        "read" => reads_data_file(input),
        CODE_MODE => true,
        _ => is_mcp(tool),
    }
}

/// The result with its output replaced by a message: `output` is dropped, so OpenCode falls back to `content`.
fn hidden(error: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("[ranon: masking failed ({error}), output hidden]") }] })
}

fn masked_result(tool: &str, result: &Value) -> Result<Option<Value>, String> {
    let mut result = result.clone();
    let mut masker = Masker::open().map_err(|e| e.to_string())?;
    let before = result.clone();
    masker.mask_json(&mut result);
    if !walk(&mut result, &mut masker) && result == before {
        return Ok(None);
    }
    masker.persist().map_err(|e| e.to_string())?;
    log(&format!("opencode:{tool}"), "masked", masker.store.issued());
    Ok(Some(result))
}

fn respond(payload: &Value) -> Result<Value, String> {
    crate::rules::load()?;
    let tool = payload["tool"].as_str().unwrap_or_default();
    match payload["event"].as_str().unwrap_or_default() {
        "after" if masks_result(tool, &payload["input"]) => match masked_result(tool, &payload["result"]) {
            Ok(Some(result)) => Ok(json!({ "result": result })),
            Ok(None) => Ok(json!({})),
            Err(error) => Err(error),
        },
        "before" if is_mcp(tool) => {
            let masker = Masker::open().map_err(|e| e.to_string())?;
            let mut input = payload["input"].clone();
            let mut restored = 0;
            restore(&mut input, &masker, &mut restored);
            Ok(if restored > 0 { json!({ "input": input }) } else { json!({}) })
        }
        _ => Ok(json!({})),
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
        // the plugin hides the result when the answer is not a result
        println!("{}", json!({ "result": hidden("unreadable hook payload") }));
        return 0;
    };
    let tool = format!("opencode:{}", payload["tool"].as_str().unwrap_or_default());
    let (error, panicked) = match catch_unwind(|| respond(&payload)) {
        Ok(Ok(output)) => {
            println!("{output}");
            return 0;
        }
        Ok(Err(error)) => (error, false),
        Err(_) => (report_panic(&tool), true),
    };
    if payload["event"] == "after" {
        if !panicked {
            log(&tool, "hidden", 0);
        }
        println!("{}", json!({ "result": hidden(&error) }));
    } else {
        // before the call: the input keeps its tokens, nothing real reaches the model
        println!("{{}}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_tools_to_mask() {
        let none = json!({});
        assert!(masks_result("crm_get_customer", &none));
        assert!(masks_result("execute", &none));
        assert!(!masks_result("bash", &none));
        assert!(!masks_result("edit", &none));
        assert!(masks_result("read", &json!({ "filePath": "/tmp/export.csv" })));
        assert!(!masks_result("read", &json!({ "filePath": "app/models/user.rb" })));
        assert!(is_mcp("notion_notion-create-pages"));
        assert!(!is_mcp("execute"));
    }
}
