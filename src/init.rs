//! `ranon init`: wires ranon into an agent harness in one command, like `rtk init`.
//!
//! - Claude Code: hooks in `.claude/settings.json` (masking, write-back, and a guard for a missing binary);
//! - OpenCode: shims, and a plugin that puts them first in the PATH of the agent's shell commands;
//! - Codex CLI: shims, the PATH of its shell commands, and the state directory made writable in its sandbox;
//! - Cursor: hooks in `.cursor/hooks.json` that run masked commands through ranon and mask MCP results.
//!
//! Existing settings are kept: ranon only adds its own entries, and `--uninstall` only removes those.

use crate::store::state_dir;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const MASK_COMMAND: &str = "ranon hook claude";
const MASK_MATCHER: &str = "Bash|Read|mcp__.*";
/// MCP tools that send text elsewhere: tokens become real values again before the call, with approval.
const WRITE_MATCHER: &str =
    "mcp__.*__.*(create|update|send|post|comment|reply|write|edit|add|type|fill|form|select|navigate|computer).*";
/// Blocks the tools whose output ranon masks while the binary is missing: Claude Code would pass them raw.
const GUARD_COMMAND: &str =
    "command -v ranon >/dev/null 2>&1 || { echo 'ranon is missing: output would not be masked' >&2; exit 2; }";
const GUARD_MARK: &str = "ranon is missing";
const PLUGIN_MARK: &str = "// installed by `ranon init`";

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Agent {
    /// Claude Code: PostToolUse and PreToolUse hooks
    Claude,
    /// OpenCode: shims and a plugin setting their PATH
    Opencode,
    /// Codex CLI: shims, shell PATH and sandbox writable root in config.toml
    Codex,
    /// Cursor: preToolUse and postToolUse hooks in hooks.json
    Cursor,
    /// Shims only, for another harness
    Shims,
}

pub struct Options {
    pub agent: Agent,
    pub global: bool,
    pub dry_run: bool,
    pub uninstall: bool,
    pub shim_dir: PathBuf,
    pub commands: Vec<String>,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

fn base(global: bool, project: &str, user: PathBuf) -> PathBuf {
    if global { user } else { std::env::current_dir().unwrap_or_default().join(project) }
}

fn claude_settings(global: bool) -> PathBuf {
    base(global, ".claude", home().join(".claude")).join("settings.json")
}

fn opencode_plugin(global: bool) -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
    base(global, ".opencode", config.join("opencode")).join("plugins").join("ranon.ts")
}

fn cursor_hooks(global: bool) -> PathBuf {
    base(global, ".cursor", home().join(".cursor")).join("hooks.json")
}

fn codex_config(global: bool) -> PathBuf {
    let user = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"));
    base(global, ".codex", user).join("config.toml")
}

pub fn run(options: &Options) -> Result<(), String> {
    let mut steps = Vec::new();
    if options.agent == Agent::Shims {
        steps.push(shims(options)?);
        print_steps(&steps, options.dry_run);
        return Ok(());
    }
    // every check runs before anything is written
    let (path, before, after) = match options.agent {
        Agent::Claude => {
            let path = claude_settings(options.global);
            let before = read(&path)?;
            let after = claude(&before, options.uninstall)?;
            (path, before, after)
        }
        Agent::Opencode => {
            let path = opencode_plugin(options.global);
            let before = read(&path)?;
            if !before.is_empty() && !before.contains(PLUGIN_MARK) {
                return Err(format!("{} exists and was not written by ranon: leaving it alone", path.display()));
            }
            let after = if options.uninstall { String::new() } else { opencode(&options.shim_dir) };
            (path, before, after)
        }
        Agent::Codex => {
            let path = codex_config(options.global);
            let before = read(&path)?;
            let after = codex(&before, &options.shim_dir, &state_dir(), options.uninstall)?;
            (path, before, after)
        }
        Agent::Cursor => {
            let path = cursor_hooks(options.global);
            let before = read(&path)?;
            let after = cursor(&before, options.uninstall)?;
            (path, before, after)
        }
        Agent::Shims => unreachable!(),
    };
    if !matches!(options.agent, Agent::Claude | Agent::Cursor) {
        steps.push(shims(options)?);
    }
    steps.push(write(&path, &before, &after, options.dry_run)?);
    print_steps(&steps, options.dry_run);
    if options.dry_run && before != after {
        println!("\n{}", diff(&before, &after));
    }
    if !options.dry_run && !options.uninstall {
        println!("\nNext: restart the agent session, then check from it that a query returning a name shows a token.");
        if options.agent == Agent::Claude {
            println!("`ranon doctor` checks the hooks from the project root.");
        }
        println!("Optional, to mask names in free text: `ranon dict build --sql \"select first_name from users …\"`.");
    }
    Ok(())
}

/// The lines `after` adds or drops, with two lines of context around each change.
fn diff(before: &str, after: &str) -> String {
    let (old, new): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    // longest common subsequence, from the end
    let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if old[i] == new[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut lines = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            lines.push((' ', old[i]));
            (i, j) = (i + 1, j + 1);
        } else if j < new.len() && (i == old.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            lines.push(('+', new[j]));
            j += 1;
        } else {
            lines.push(('-', old[i]));
            i += 1;
        }
    }
    let near_change = |at: usize| lines[at.saturating_sub(2)..(at + 3).min(lines.len())].iter().any(|(k, _)| *k != ' ');
    let mut out = Vec::new();
    for (at, (kind, line)) in lines.iter().enumerate() {
        if near_change(at) {
            out.push(format!("{kind} {line}"));
        } else if out.last().is_none_or(|last: &String| last != "  …") {
            out.push("  …".to_string());
        }
    }
    out.join("\n")
}

fn print_steps(steps: &[String], dry_run: bool) {
    let prefix = if dry_run { "would " } else { "" };
    for step in steps {
        println!("{prefix}{step}");
    }
}

fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn write(path: &Path, before: &str, after: &str, dry_run: bool) -> Result<String, String> {
    if before == after {
        return Ok(format!("keep {} (already up to date)", path.display()));
    }
    let action = if after.is_empty() {
        "remove"
    } else if before.is_empty() {
        "create"
    } else {
        "update"
    };
    if !dry_run {
        if after.is_empty() {
            std::fs::remove_file(path).map_err(|error| format!("{}: {error}", path.display()))?;
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
            }
            std::fs::write(path, after).map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    Ok(format!("{action} {}", path.display()))
}

fn shims(options: &Options) -> Result<String, String> {
    let dir = &options.shim_dir;
    let names = options.commands.join(", ");
    if options.dry_run {
        let verb = if options.uninstall { "keep" } else { "link" };
        return Ok(format!("{verb} shims in {} for {names}", dir.display()));
    }
    if options.uninstall {
        // shims are shared by harnesses: removing them is left to the user
        return Ok(format!("keep shims in {} (shared by harnesses; delete the directory to drop them)", dir.display()));
    }
    let exe = std::env::current_exe().map_err(|_| "cannot locate the ranon binary".to_string())?;
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for command in &options.commands {
        let link = dir.join(command);
        std::fs::remove_file(&link).ok();
        std::os::unix::fs::symlink(&exe, &link).map_err(|error| format!("{}: {error}", link.display()))?;
    }
    Ok(format!("link shims in {} for {names} -> {}", dir.display(), exe.display()))
}

// --- Claude Code ------------------------------------------------------------------------------------------

fn claude(text: &str, uninstall: bool) -> Result<String, String> {
    let mut settings: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text)
            .map_err(|error| format!("settings file is not valid JSON ({error}): fix it first"))?
    };
    if !settings.is_object() {
        return Err("settings file is not a JSON object".to_string());
    }
    if uninstall {
        remove_hooks(&mut settings);
        if settings.as_object().is_some_and(serde_json::Map::is_empty) && !text.trim().is_empty() {
            return Ok(String::new());
        }
    } else {
        add_hook(&mut settings, "PostToolUse", MASK_MATCHER, MASK_COMMAND, "ranon hook");
        add_hook(&mut settings, "PreToolUse", WRITE_MATCHER, MASK_COMMAND, "ranon hook");
        add_hook(&mut settings, "PreToolUse", MASK_MATCHER, GUARD_COMMAND, GUARD_MARK);
    }
    if text.trim().is_empty() && settings.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(String::new());
    }
    let mut out = serde_json::to_string_pretty(&settings).map_err(|error| error.to_string())?;
    out.push('\n');
    // a file that only differs in formatting is left as it is
    let unchanged = serde_json::from_str::<Value>(text).ok().is_some_and(|before| before == settings);
    Ok(if unchanged { text.to_string() } else { out })
}

fn runs(entry: &Value, mark: &str) -> bool {
    entry["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|hook| hook["command"].as_str().is_some_and(|command| command.contains(mark)))
    })
}

/// Adds the hook unless one of the event already runs it, whatever its matcher: a user's matcher is kept.
fn add_hook(settings: &mut Value, event: &str, matcher: &str, command: &str, mark: &str) {
    let hooks = settings.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return };
    let entries = hooks.entry(event).or_insert_with(|| json!([]));
    let Some(entries) = entries.as_array_mut() else { return };
    if entries.iter().any(|entry| runs(entry, mark)) {
        return;
    }
    entries.push(json!({ "matcher": matcher, "hooks": [{ "type": "command", "command": command }] }));
}

fn remove_hooks(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return };
    for entries in hooks.values_mut() {
        let Some(entries) = entries.as_array_mut() else { continue };
        for entry in entries.iter_mut() {
            if let Some(list) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                list.retain(|hook| {
                    let command = hook["command"].as_str().unwrap_or_default();
                    !(command.contains("ranon hook") || command.contains(GUARD_MARK))
                });
            }
        }
        entries.retain(|entry| entry["hooks"].as_array().is_none_or(|list| !list.is_empty()));
    }
    hooks.retain(|_, entries| entries.as_array().is_none_or(|list| !list.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().unwrap().remove("hooks");
    }
}

// --- Cursor ---------------------------------------------------------------------------------------------

const CURSOR_COMMAND: &str = "ranon hook cursor";

fn cursor(text: &str, uninstall: bool) -> Result<String, String> {
    let mut config: Value = if text.trim().is_empty() {
        json!({ "version": 1 })
    } else {
        serde_json::from_str(text).map_err(|error| format!("hooks.json is not valid JSON ({error}): fix it first"))?
    };
    if !config.is_object() {
        return Err("hooks.json is not a JSON object".to_string());
    }
    let ours = |entry: &Value| entry["command"].as_str().is_some_and(|command| command.contains("ranon hook"));
    if uninstall {
        if let Some(hooks) = config.get_mut("hooks").and_then(Value::as_object_mut) {
            for entries in hooks.values_mut() {
                if let Some(entries) = entries.as_array_mut() {
                    entries.retain(|entry| !ours(entry));
                }
            }
            hooks.retain(|_, entries| entries.as_array().is_none_or(|list| !list.is_empty()));
            if hooks.is_empty() {
                config.as_object_mut().unwrap().remove("hooks");
            }
        }
        let only_version = config.as_object().unwrap().keys().all(|key| key == "version");
        if only_version {
            return Ok(String::new());
        }
    } else {
        // shell commands are rewritten before they run; MCP inputs get real values back, MCP results are masked.
        // failClosed: a crashed or missing ranon blocks the call instead of letting it through
        for (event, matcher) in [("preToolUse", "Shell|MCP:.*"), ("postToolUse", "MCP:.*")] {
            let hooks = config.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
            let entries = hooks
                .as_object_mut()
                .ok_or("hooks.json: `hooks` is not an object")?
                .entry(event)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or_else(|| format!("hooks.json: `{event}` is not an array"))?;
            if !entries.iter().any(ours) {
                entries.push(json!({ "command": CURSOR_COMMAND, "matcher": matcher, "failClosed": true }));
            }
        }
    }
    let unchanged = serde_json::from_str::<Value>(text).ok().is_some_and(|before| before == config);
    if unchanged {
        return Ok(text.to_string());
    }
    let mut out = serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?;
    out.push('\n');
    Ok(out)
}

// --- OpenCode ---------------------------------------------------------------------------------------------

/// The plugin: shims first in the PATH of shell commands, and every tool call sent to `ranon hook opencode`,
/// which masks results and puts real values back in MCP inputs. OpenCode's own tools other than `read` are
/// skipped here, so that a missing ranon binary hides only the results it should have masked.
const OPENCODE_PLUGIN: &str = r#"__MARK__: masks tool results (MCP tools, Code Mode, data files) and puts the ranon shims first in the PATH of the agent's shell commands
import { existsSync } from "node:fs"
import { spawnSync } from "node:child_process"

const shims = __SHIMS__
const ranon = __RANON__
const builtIn = new Set(__BUILT_IN__)
const skipped = (tool: string) => builtIn.has(tool) && tool !== "read"

function call(payload: unknown): any {
  const run = spawnSync(ranon, ["hook", "opencode"], { input: JSON.stringify(payload), encoding: "utf8", maxBuffer: 256 * 1024 * 1024 })
  if (run.error || run.status !== 0) throw new Error(run.error ? "ranon not found" : `ranon exited with ${run.status}`)
  return JSON.parse(run.stdout)
}

// in place: OpenCode reads the object it passed to the hook
function replace(target: Record<string, unknown>, value: Record<string, unknown>) {
  for (const key of Object.keys(target)) delete target[key]
  Object.assign(target, value)
}

export default {
  id: "ranon",
  async setup(ctx: any) {
    if (existsSync(shims)) {
      await ctx.shell.hook("create.before", (event: { env?: Record<string, string | undefined> }) => {
        if (!event.env) return
        const path = event.env.PATH ?? process.env.PATH ?? ""
        if (path.split(":").includes(shims)) return
        event.env.PATH = `${shims}:${path}`
      })
    }
    if (!ctx.tool?.hook) return
    await ctx.tool.hook("execute.before", (event: any) => {
      if (skipped(event.tool) || !event.input || typeof event.input !== "object") return
      try {
        const answer = call({ event: "before", tool: event.tool, input: event.input })
        if (answer.input) replace(event.input, answer.input)
      } catch {
        // the input keeps its tokens: nothing real reaches the model
      }
    })
    await ctx.tool.hook("execute.after", (event: any) => {
      if (skipped(event.tool) || event.status !== "completed" || !event.result) return
      let answer: any
      try {
        answer = call({ event: "after", tool: event.tool, input: event.input, result: event.result })
      } catch (error) {
        // fail closed: hide the result rather than pass it raw
        answer = { result: { content: [{ type: "text", text: `[ranon: masking failed (${(error as Error).message}), output hidden]` }] } }
      }
      if (answer.result) replace(event.result, answer.result)
    })
  },
}
"#;

fn opencode(shim_dir: &Path) -> String {
    let ranon = std::env::current_exe().map(|exe| exe.display().to_string()).unwrap_or_else(|_| "ranon".to_string());
    let json = |text: &str| serde_json::to_string(text).unwrap();
    OPENCODE_PLUGIN
        .replace("__MARK__", PLUGIN_MARK)
        .replace("__SHIMS__", &json(&shim_dir.display().to_string()))
        .replace("__RANON__", &json(&ranon))
        .replace("__BUILT_IN__", &serde_json::to_string(crate::opencode::BUILT_IN).unwrap())
}

// --- Codex CLI --------------------------------------------------------------------------------------------

fn codex(text: &str, shim_dir: &Path, state: &Path, uninstall: bool) -> Result<String, String> {
    use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};
    let mut doc: DocumentMut =
        text.parse().map_err(|error| format!("config.toml is not valid TOML ({error}): fix it first"))?;
    let shims = shim_dir.display().to_string();
    let state = state.display().to_string();
    // each directory once, in its first place, and never the shims
    let without_shims = |path: &str| {
        let mut dirs: Vec<&str> = Vec::new();
        for dir in path.split(':').filter(|dir| !dir.is_empty() && *dir != shims) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs.join(":")
    };
    let env_path = without_shims(&std::env::var("PATH").unwrap_or_default());

    if uninstall {
        if let Some(set) = doc
            .get_mut("shell_environment_policy")
            .and_then(|policy| policy.get_mut("set"))
            .and_then(Item::as_table_like_mut)
        {
            if let Some(path) = set.get("PATH").and_then(Item::as_str).map(str::to_string) {
                let rest = without_shims(&path);
                if rest == env_path || rest.is_empty() {
                    set.remove("PATH");
                } else if rest != path {
                    set.insert("PATH", value(rest));
                }
            }
            if set.is_empty() {
                doc["shell_environment_policy"].as_table_like_mut().unwrap().remove("set");
            }
        }
        if let Some(roots) = doc
            .get_mut("sandbox_workspace_write")
            .and_then(|table| table.get_mut("writable_roots"))
            .and_then(Item::as_array_mut)
        {
            roots.retain(|root| root.as_str() != Some(state.as_str()));
            if roots.is_empty() {
                doc["sandbox_workspace_write"].as_table_like_mut().unwrap().remove("writable_roots");
            }
        }
        for key in ["shell_environment_policy", "sandbox_workspace_write"] {
            if doc.get(key).and_then(Item::as_table_like).is_some_and(|table| table.is_empty()) {
                doc.remove(key);
            }
        }
        return Ok(doc.to_string());
    }

    let policy = doc.entry("shell_environment_policy").or_insert_with(|| Item::Table(Table::new()));
    let policy = policy.as_table_like_mut().ok_or("shell_environment_policy is not a table")?;
    if policy.get("set").is_none() {
        policy.insert("set", Item::Value(InlineTable::new().into()));
    }
    let set =
        policy.get_mut("set").and_then(Item::as_table_like_mut).ok_or("shell_environment_policy.set is not a table")?;
    let current = set.get("PATH").and_then(Item::as_str).map(str::to_string).unwrap_or(env_path);
    if current.split(':').next() != Some(shims.as_str()) {
        // `set` replaces PATH rather than prepending to it: the whole value is written
        set.insert("PATH", value(format!("{shims}:{}", without_shims(&current))));
    }

    let sandbox = doc.entry("sandbox_workspace_write").or_insert_with(|| Item::Table(Table::new()));
    let sandbox = sandbox.as_table_like_mut().ok_or("sandbox_workspace_write is not a table")?;
    if sandbox.get("writable_roots").is_none() {
        sandbox.insert("writable_roots", value(Array::new()));
    }
    let roots = sandbox
        .get_mut("writable_roots")
        .and_then(Item::as_array_mut)
        .ok_or("sandbox_workspace_write.writable_roots is not an array")?;
    // without it the sandbox stops ranon from saving the token map, and every new value hides the output
    if !roots.iter().any(|root| root.as_str() == Some(state.as_str())) {
        roots.push(state);
    }
    Ok(doc.to_string())
}

// --- show -------------------------------------------------------------------------------------------------

pub fn show(shim_dir: &Path) -> i32 {
    for global in [false, true] {
        let scope = if global { "global" } else { "this project" };
        let settings = read(&claude_settings(global)).unwrap_or_default();
        let parsed: Value = serde_json::from_str(&settings).unwrap_or(Value::Null);
        let claude = ["PostToolUse", "PreToolUse"].iter().any(|event| {
            parsed["hooks"][event].as_array().is_some_and(|list| list.iter().any(|e| runs(e, "ranon hook")))
        });
        let plugin = read(&opencode_plugin(global)).unwrap_or_default();
        let opencode = if plugin.contains(PLUGIN_MARK) {
            "installed"
        } else if plugin.contains("shims") {
            "installed by hand"
        } else {
            "-"
        };
        let codex_text = read(&codex_config(global)).unwrap_or_default();
        let codex = codex_text.contains(&shim_dir.display().to_string());
        let cursor = read(&cursor_hooks(global)).unwrap_or_default().contains(CURSOR_COMMAND);
        let mark = |on: bool| if on { "installed" } else { "-" };
        println!("{scope}:");
        println!("  claude    {:<17} {}", mark(claude), claude_settings(global).display());
        println!("  opencode  {opencode:<17} {}", opencode_plugin(global).display());
        println!("  codex     {:<17} {}", mark(codex), codex_config(global).display());
        println!("  cursor    {:<17} {}", mark(cursor), cursor_hooks(global).display());
    }
    let links: Vec<String> = std::fs::read_dir(shim_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    println!(
        "shims in {}: {}",
        shim_dir.display(),
        if links.is_empty() { "none".to_string() } else { links.join(", ") }
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_hooks_are_added_once_and_removed() {
        let added = claude("", false).unwrap();
        let settings: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(settings["hooks"]["PostToolUse"][0]["matcher"], MASK_MATCHER);
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(claude(&added, false).unwrap(), added, "a second run changes nothing");
        assert_eq!(claude(&added, true).unwrap(), "");
    }

    #[test]
    fn claude_keeps_other_settings_and_a_custom_matcher() {
        let mine = r#"{"model": "x", "hooks": {"PostToolUse": [{"matcher": "Bash|mcp__notion__.*",
            "hooks": [{"type": "command", "command": "ranon hook claude"}]},
            {"matcher": "Edit", "hooks": [{"type": "command", "command": "fmt.sh"}]}]}}"#;
        let added: Value = serde_json::from_str(&claude(mine, false).unwrap()).unwrap();
        assert_eq!(added["model"], "x");
        let post = added["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2, "the user's masking matcher is kept, none added: {post:?}");
        let removed: Value = serde_json::from_str(&claude(&added.to_string(), true).unwrap()).unwrap();
        assert_eq!(removed["model"], "x");
        assert_eq!(removed["hooks"]["PostToolUse"].as_array().unwrap().len(), 1, "fmt.sh stays");
        assert!(removed["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn claude_refuses_invalid_json() {
        assert!(claude("{ not json", false).is_err());
    }

    #[test]
    fn codex_config_gets_path_and_writable_root_once() {
        let mine = "# my settings\nmodel = \"gpt\"\n";
        let shims = Path::new("/h/.local/share/ranon/shims");
        let state = Path::new("/h/.local/state/ranon");
        let added = codex(mine, shims, state, false).unwrap();
        assert!(added.starts_with("# my settings\nmodel = \"gpt\"\n"), "{added}");
        let doc: toml::Table = toml::from_str(&added).unwrap();
        let path = doc["shell_environment_policy"]["set"]["PATH"].as_str().unwrap();
        assert!(path.starts_with("/h/.local/share/ranon/shims:"), "{path}");
        assert_eq!(doc["sandbox_workspace_write"]["writable_roots"][0].as_str(), Some("/h/.local/state/ranon"));
        assert_eq!(codex(&added, shims, state, false).unwrap(), added, "a second run changes nothing");
        assert_eq!(codex(&added, shims, state, true).unwrap().trim(), mine.trim());
    }

    #[test]
    fn codex_prepends_shims_to_a_path_already_set() {
        let mine = "[shell_environment_policy]\nset = { PATH = \"/opt/bin:/usr/bin\", FOO = \"1\" }\n";
        let added = codex(mine, Path::new("/s"), Path::new("/st"), false).unwrap();
        let doc: toml::Table = toml::from_str(&added).unwrap();
        assert_eq!(doc["shell_environment_policy"]["set"]["PATH"].as_str(), Some("/s:/opt/bin:/usr/bin"));
        let removed: toml::Table =
            toml::from_str(&codex(&added, Path::new("/s"), Path::new("/st"), true).unwrap()).unwrap();
        assert_eq!(removed["shell_environment_policy"]["set"]["PATH"].as_str(), Some("/opt/bin:/usr/bin"));
        assert_eq!(removed["shell_environment_policy"]["set"]["FOO"].as_str(), Some("1"));
    }

    #[test]
    fn opencode_plugin_embeds_the_shim_dir() {
        let plugin = opencode(Path::new("/h/shims"));
        assert!(plugin.starts_with(PLUGIN_MARK));
        assert!(plugin.contains(r#"const shims = "/h/shims""#));
    }

    #[test]
    fn cursor_adds_its_hooks_keeps_others_and_undoes_it() {
        let mine = r#"{"version": 1, "hooks": {"stop": [{"command": "./audit.sh"}]}}"#;
        let added = cursor(mine, false).unwrap();
        let config: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(config["hooks"]["stop"][0]["command"], "./audit.sh");
        assert_eq!(config["hooks"]["preToolUse"][0]["matcher"], "Shell|MCP:.*");
        assert_eq!(config["hooks"]["postToolUse"][0]["failClosed"], true);
        assert_eq!(cursor(&added, false).unwrap(), added);
        let removed: Value = serde_json::from_str(&cursor(&added, true).unwrap()).unwrap();
        assert_eq!(removed, serde_json::from_str::<Value>(mine).unwrap());

        assert_eq!(cursor(&cursor("", false).unwrap(), true).unwrap(), "");
    }
}
