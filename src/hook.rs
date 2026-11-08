//! Claude Code PostToolUse hook: replaces the tool output with its masked version.

use crate::mask::Masker;
use regex::Regex;
use serde_json::{Value, json};
use std::io::Read;
use std::sync::OnceLock;

fn masks_bash(command: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(^|[\s;&|(])(psql|heroku\s+run)(\s|$)").unwrap()).is_match(command)
}

fn psql_tuples_only(command: &str) -> bool {
    static PSQL: OnceLock<Regex> = OnceLock::new();
    static TUPLES: OnceLock<Regex> = OnceLock::new();
    PSQL.get_or_init(|| Regex::new(r"(^|[\s;&|(])psql(\s|$)").unwrap()).is_match(command)
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
    let mut response = payload["tool_response"].clone();
    if payload["tool_name"] == "Bash" {
        let command = payload["tool_input"]["command"].as_str().unwrap_or_default();
        if !masks_bash(command) {
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
    if !walk(&mut response, &mut masker) {
        return Ok(None);
    }
    masker.persist().map_err(|e| e.to_string())?;
    Ok(Some(response))
}

pub fn claude() -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&input) else { return 0 };
    let updated = match masked_response(&payload) {
        Ok(Some(response)) => response,
        Ok(None) => return 0,
        // fail closed: hide the output rather than leak it
        Err(error) => {
            json!({ "stdout": format!("[ranon: masking failed ({error}), output hidden]"), "stderr": "", "interrupted": false, "isImage": false })
        }
    };
    println!("{}", json!({ "hookSpecificOutput": { "hookEventName": "PostToolUse", "updatedToolOutput": updated } }));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_psql_and_heroku_run_only() {
        assert!(masks_bash("psql -c 'select 1'"));
        assert!(masks_bash("PGOPTIONS='-c statement_timeout=30s' psql \"$URL\" -c q"));
        assert!(masks_bash("rtk psql -c q"));
        assert!(masks_bash("heroku run \"rails c --sandbox\" -a my-app"));
        assert!(!masks_bash("cat privacy-psql.py"));
        assert!(!masks_bash("git status"));
    }

    #[test]
    fn detects_psql_tuples_only() {
        assert!(psql_tuples_only("psql -At -c q"));
        assert!(!psql_tuples_only("psql --set x=1 -c q"));
        assert!(!psql_tuples_only("ls -t"));
    }
}
