//! `ranon doctor`: checks that masking is actually in place on this machine.

use crate::mask::Masker;
use crate::names::Names;
use crate::store::{Store, state_dir};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

enum Check {
    Ok(String),
    Warn(String),
    Fail(String),
}

/// `ranon` as the hooks will find it: first in PATH, and this very binary.
fn binary() -> Check {
    let me = std::env::current_exe().ok().and_then(|path| path.canonicalize().ok());
    let in_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("ranon"))
        .find(|candidate| candidate.is_file());
    match in_path {
        None => Check::Fail("ranon is not in PATH: a hook whose binary is missing lets output through".to_string()),
        Some(path) if path.canonicalize().ok() != me => {
            Check::Warn(format!("PATH resolves ranon to {}, not to this binary", path.display()))
        }
        Some(path) => Check::Ok(format!("binary: {}", path.display())),
    }
}

fn rules() -> Check {
    match crate::rules::load() {
        Ok(()) => Check::Ok(format!("rules: loaded from {}", crate::rules::config_dir().display())),
        Err(error) => Check::Fail(format!("rules: {error} (every output is hidden until this is fixed)")),
    }
}

/// State files readable by their owner only.
fn permissions() -> Check {
    let dir = state_dir();
    let exposed: Vec<String> = std::iter::once(dir.clone())
        .chain(["salt", "map.tsv", "names.bin", "hook.log"].map(|name| dir.join(name)))
        .filter(|path| path.metadata().is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0))
        .map(|path| path.display().to_string())
        .collect();
    if exposed.is_empty() {
        Check::Ok(format!("state: {} is private", dir.display()))
    } else {
        Check::Fail(format!("state: readable by other users, run chmod go-rwx on {}", exposed.join(" ")))
    }
}

fn token_map(store: &Store) -> Check {
    Check::Ok(format!("token map: {} real values kept in map.tsv (`ranon forget` drops them)", store.len()))
}

fn dictionary(store: &Store) -> Check {
    match Names::load(store) {
        Some(names) => Check::Ok(format!("name dictionary: {} hashed words", names.len())),
        None => {
            Check::Warn("name dictionary: none, names in free text are not masked (`ranon dict build`)".to_string())
        }
    }
}

/// Masks a synthetic sample end to end.
fn self_test(masker: &mut Masker) -> Check {
    let sample = " id | first_name | email\n----+------------+-------\n 1 | Camille | camille@example.com\n";
    let masked = masker.mask_text(sample);
    if masked.contains("Camille") || masked.contains("camille@example.com") {
        Check::Fail("self-test: a sample table came out unmasked".to_string())
    } else {
        Check::Ok("self-test: sample table masked".to_string())
    }
}

/// `.claude/settings*.json` of the current directory and its parents, then of the home directory.
fn settings_files() -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    if !dirs.contains(&home.as_path()) {
        dirs.push(&home);
    }
    dirs.iter()
        .flat_map(|dir| ["settings.json", "settings.local.json"].map(|name| dir.join(".claude").join(name)))
        .filter(|path| path.is_file())
        .collect()
}

/// Matchers of the `event` hooks that run ranon in a Claude Code settings file.
fn hook_matchers(settings: &Value, event: &str) -> Vec<String> {
    let runs_ranon = |entry: &Value| {
        entry["hooks"].as_array().is_some_and(|hooks| {
            hooks.iter().any(|hook| hook["command"].as_str().is_some_and(|command| command.contains("ranon hook")))
        })
    };
    settings["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| runs_ranon(entry))
        .map(|entry| entry["matcher"].as_str().unwrap_or("*").to_string())
        .collect()
}

fn claude_hooks() -> Vec<Check> {
    let mut checks = Vec::new();
    for path in settings_files() {
        let Some(settings) =
            std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        for (event, role) in [("PostToolUse", "masks"), ("PreToolUse", "unmasks writes of")] {
            for matcher in hook_matchers(&settings, event) {
                checks.push(Check::Ok(format!("Claude Code hook in {}: {role} {matcher}", path.display())));
            }
        }
    }
    if checks.is_empty() {
        checks.push(Check::Warn("Claude Code hook: none found in .claude/settings.json here or in ~".to_string()));
    }
    checks
}

/// What the hook did so far, from `hook.log`.
fn activity() -> Check {
    let log = std::fs::read_to_string(state_dir().join("hook.log")).unwrap_or_default();
    let count = |outcome: &str| log.lines().filter(|line| line.split('\t').nth(2) == Some(outcome)).count();
    let values: usize = log.lines().filter_map(|line| line.split('\t').nth(3)?.parse::<usize>().ok()).sum();
    Check::Ok(format!(
        "hook activity: {} outputs masked ({values} values), {} hidden",
        count("masked"),
        count("hidden")
    ))
}

pub fn run() -> i32 {
    let mut checks = vec![binary(), rules()];
    match Masker::open() {
        Ok(mut masker) => {
            checks.extend([permissions(), token_map(&masker.store), dictionary(&masker.store), self_test(&mut masker)])
        }
        Err(error) => checks.push(Check::Fail(format!("state: {error}"))),
    }
    checks.extend(claude_hooks());
    checks.push(activity());
    let mut failed = false;
    for check in &checks {
        match check {
            Check::Ok(message) => println!("ok    {message}"),
            Check::Warn(message) => println!("warn  {message}"),
            Check::Fail(message) => {
                failed = true;
                println!("FAIL  {message}");
            }
        }
    }
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_ranon_hooks_and_their_matchers() {
        let settings = json!({ "hooks": { "PostToolUse": [
            { "matcher": "Bash|mcp__slack__.*", "hooks": [{ "type": "command", "command": "ranon hook claude" }] },
            { "matcher": "Edit", "hooks": [{ "type": "command", "command": "lint.sh" }] },
            { "hooks": [{ "type": "command", "command": "/usr/local/bin/ranon hook claude" }] },
        ] } });
        assert_eq!(hook_matchers(&settings, "PostToolUse"), ["Bash|mcp__slack__.*", "*"]);
        assert!(hook_matchers(&settings, "PreToolUse").is_empty());
    }
}
