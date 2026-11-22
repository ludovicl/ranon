//! Each agent, simulated as closely as the CI allows without calling a model:
//! - Claude Code: payloads captured from a real session (synthetic data, ids and paths replaced) go
//!   through the hook, so a change in their shape shows up here.
//! - Codex CLI and OpenCode: `ranon init` writes their config, the test runs a shell the way the agent
//!   would with that config, and a fake `psql` behind the shims must come out masked.
//! - OpenCode tools: the plugin runs under Node and its tool hooks get MCP results and inputs shaped like
//!   the ones a real session passes.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_ranon");
const SECRETS: &[&str] = &["Jean", "Dupont", "jean.dupont@example.com"];

fn home() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ranon-harness-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the ranon binary (or a shell) with `home` holding HOME and ranon's state and config.
fn command(program: &str, home: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env("HOME", home)
        .env("RANON_HOME", home.join("state"))
        .env("RANON_CONFIG_DIR", home.join("config"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CODEX_HOME");
    command
}

fn stdout(command: &mut Command) -> String {
    let out = command.output().unwrap();
    let (stdout, stderr) = (String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap());
    assert!(out.status.success(), "{stdout}{stderr}");
    stdout
}

fn assert_hidden(output: &str) {
    for secret in SECRETS {
        assert!(!output.contains(secret), "`{secret}` leaked in:\n{output}");
    }
}

// --- Claude Code --------------------------------------------------------------------------------

fn fixture(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code").join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn hook(home: &Path, payload: &serde_json::Value) -> serde_json::Value {
    let mut child = command(BIN, home)
        .args(["hook", "claude"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.to_string().as_bytes()).unwrap();
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    serde_json::from_str::<serde_json::Value>(&out).unwrap_or_else(|_| panic!("no hook output for {payload}"))
        ["hookSpecificOutput"]
        .clone()
}

/// The output keeps every field of the tool response, with the personal data masked.
fn assert_masked_like(payload: &serde_json::Value, updated: &serde_json::Value) {
    assert_hidden(&updated.to_string());
    assert!(updated.to_string().contains("ranon-fixture"), "{updated}");
    let original = &payload["tool_response"];
    match original {
        serde_json::Value::Object(fields) => {
            for key in fields.keys() {
                assert!(updated.get(key).is_some(), "`{key}` dropped from {updated}");
            }
        }
        serde_json::Value::Array(blocks) => assert_eq!(updated.as_array().unwrap().len(), blocks.len()),
        _ => panic!("unexpected tool_response {original}"),
    }
}

#[test]
fn claude_code_bash_payload() {
    let payload = fixture("post-bash.json");
    let output = hook(&home(), &payload);
    assert_masked_like(&payload, &output["updatedToolOutput"]);
}

#[test]
fn claude_code_read_payload() {
    let payload = fixture("post-read.json");
    let output = hook(&home(), &payload);
    let updated = &output["updatedToolOutput"];
    assert_masked_like(&payload, updated);
    assert_eq!(updated["file"]["filePath"], payload["tool_response"]["file"]["filePath"]);
    assert_eq!(updated["file"]["numLines"], payload["tool_response"]["file"]["numLines"]);
}

#[test]
fn claude_code_browser_payload() {
    let payload = fixture("post-browser.json");
    // a page is free text: names are recognized from the dictionary built from the users table
    let home = home();
    let names = write(&home.join("names.txt"), "Jean\nDupont\n");
    stdout(command(BIN, &home).args(["dict", "build", "--stdin"]).stdin(std::fs::File::open(names).unwrap()));
    let output = hook(&home, &payload);
    assert_masked_like(&payload, &output["updatedToolOutput"]);
}

#[test]
fn claude_code_browser_input_gets_real_values_back() {
    let home = home();
    // the token of `Jean` in this test's state, as the model would have read it
    let masked = stdout(
        command(BIN, &home)
            .args(["mask"])
            .stdin(std::fs::File::open(write(&home.join("in.txt"), "first_name: Jean\n")).unwrap()),
    );
    let token = masked.trim().strip_prefix("first_name: ").unwrap();
    assert!(token.starts_with("PERSON_"), "{masked}");

    let mut payload = fixture("pre-browser-form-input.json");
    let value = payload["tool_input"]["value"].as_str().unwrap().replace("PERSON_d26f1d", token);
    payload["tool_input"]["value"] = value.into();
    let output = hook(&home, &payload);
    assert_eq!(output["permissionDecision"], "ask");
    assert_eq!(output["updatedInput"]["value"], "ranon-fixture Jean");
    assert_eq!(output["updatedInput"]["ref"], payload["tool_input"]["ref"]);
}

// --- Codex CLI and OpenCode ---------------------------------------------------------------------

fn write(path: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    path.to_path_buf()
}

/// A fake `psql` printing a synthetic row, and the PATH the agent had before ranon.
fn fake_psql(home: &Path) -> String {
    let psql = write(
        &home.join("bin/psql"),
        "#!/bin/sh\nprintf ' first_name | last_name |          email\\n------------+-----------+-------------------------\\n Jean       | Dupont    | jean.dupont@example.com\\n(1 row)\\n'\n",
    );
    std::fs::set_permissions(&psql, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:/usr/bin:/bin", home.join("bin").display())
}

/// What the agent's shell prints for `psql`, run with `path`.
fn psql_through(home: &Path, path: &str) -> String {
    let out = stdout(
        command("sh", home).args(["-c", "psql -c 'select first_name, last_name, email from users'"]).env("PATH", path),
    );
    assert!(out.contains("PERSON_") && out.contains("EMAIL_"), "{out}");
    out
}

/// `ranon init -g --agent codex` with a fake `psql` on the PATH it records.
fn codex_home() -> PathBuf {
    let home = home();
    let path = fake_psql(&home);
    stdout(
        command(BIN, &home)
            .args(["init", "-g", "--agent", "codex", "--shim-dir"])
            .arg(home.join("shims"))
            .arg("psql")
            .env("PATH", &path),
    );
    home
}

#[test]
fn codex_shell_commands_are_masked() {
    let home = codex_home();
    // Codex replaces the PATH of its shell commands with this value
    let config: toml_edit::DocumentMut =
        std::fs::read_to_string(home.join(".codex/config.toml")).unwrap().parse().unwrap();
    let codex_path = config["shell_environment_policy"]["set"]["PATH"].as_str().unwrap().to_string();
    assert_hidden(&psql_through(&home, &codex_path));
}

/// Runs `psql` in Codex's own sandbox (`codex sandbox`, no model involved), with the config ranon wrote
/// and the temporary directories made read-only, so the state directory is writable only through
/// `writable_roots`.
fn psql_in_codex_sandbox(codex: &Path, home: &Path, overrides: &[&str]) -> String {
    let mut sandbox = command(codex.to_str().unwrap(), home);
    sandbox.env("CODEX_HOME", home.join(".codex")).args(["sandbox", "-c", "sandbox_mode=\"workspace-write\""]);
    for option in ["exclude_slash_tmp=true", "exclude_tmpdir_env_var=true"].iter().chain(overrides) {
        sandbox.args(["-c", &format!("sandbox_workspace_write.{option}")]);
    }
    let out = sandbox.args(["--", "sh", "-c", "psql -c q"]).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn codex_sandbox_lets_ranon_save_its_tokens() {
    let Some(codex) = required("codex", "RANON_REQUIRE_CODEX") else { return };
    let home = codex_home();
    let out = psql_in_codex_sandbox(&codex, &home, &[]);
    assert_hidden(&out);
    assert!(out.contains("PERSON_") && out.contains("EMAIL_"), "{out}");

    // without the writable root ranon cannot save new tokens: it hides the output instead of leaking it
    std::fs::remove_file(home.join("state/map.tsv")).unwrap();
    let out = psql_in_codex_sandbox(&codex, &home, &["writable_roots=[]"]);
    assert_hidden(&out);
    assert!(out.contains("output hidden"), "{out}");
}

#[test]
fn opencode_shell_commands_are_masked() {
    let Some(node) = required("node", "CI") else { return };
    let home = home();
    let path = fake_psql(&home);
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    stdout(
        command(BIN, &home)
            .args(["init", "--agent", "opencode", "--shim-dir"])
            .arg(home.join("shims"))
            .arg("psql")
            .current_dir(&project),
    );

    // load the plugin as OpenCode does, and fire its hook on a new shell with the agent's PATH
    let driver = r#"
        import { pathToFileURL } from "node:url"
        const [plugin, path] = process.argv.slice(1)
        const { default: ranon } = await import(pathToFileURL(plugin).href)
        let hook
        await ranon.setup({ shell: { hook: async (name, fn) => { if (name === "create.before") hook = fn } } })
        const event = { env: { PATH: path } }
        await hook(event)
        process.stdout.write(event.env.PATH)
    "#;
    let plugin = project.join(".opencode/plugins/ranon.ts");
    let opencode_path = stdout(Command::new(node).args(["--input-type=module", "-e", driver]).arg(&plugin).arg(&path));
    assert_hidden(&psql_through(&home, &opencode_path));
}

/// Fires the plugin's tool hooks the way OpenCode does (checked in a real session with an MCP server, OpenCode
/// 2.3.2, 22/11/2026: the hooks get MCP calls, also inside Code Mode, and OpenCode reads the objects back).
fn opencode_tool_events(home: &Path, plugin: &Path, events: &serde_json::Value) -> serde_json::Value {
    let node = required("node", "CI").unwrap();
    let driver = r#"
        import { pathToFileURL } from "node:url"
        const [plugin, events] = process.argv.slice(1)
        const { default: ranon } = await import(pathToFileURL(plugin).href)
        const hooks = {}
        await ranon.setup({ shell: { hook: async () => {} }, tool: { hook: async (name, fn) => { hooks[name] = fn } } })
        const fired = JSON.parse(events)
        for (const event of fired) await hooks[`execute.${event.when}`](event)
        process.stdout.write(JSON.stringify(fired))
    "#;
    let mut node = command(node.to_str().unwrap(), home);
    let out = stdout(node.args(["--input-type=module", "-e", driver]).arg(plugin).arg(events.to_string()));
    serde_json::from_str(&out).unwrap()
}

#[test]
fn opencode_tool_results_are_masked_and_inputs_restored() {
    if required("node", "CI").is_none() {
        return;
    }
    let home = home();
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    stdout(
        command(BIN, &home)
            .args(["init", "--agent", "opencode", "--shim-dir"])
            .arg(home.join("shims"))
            .current_dir(&project),
    );
    let plugin = project.join(".opencode/plugins/ranon.ts");

    let customer = serde_json::json!({ "first_name": "Jean", "last_name": "Dupont", "email": "jean.dupont@example.com", "plan": "pro" });
    let result =
        serde_json::json!({ "output": customer, "content": [{ "type": "text", "text": customer.to_string() }] });
    let edit = serde_json::json!({ "output": "Jean Dupont", "content": [{ "type": "text", "text": "Jean Dupont" }] });
    let events = serde_json::json!([
        { "when": "after", "tool": "crm_get_customer", "status": "completed", "input": { "id": 42 }, "result": result },
        { "when": "after", "tool": "edit", "status": "completed", "input": {}, "result": edit },
    ]);
    let fired = opencode_tool_events(&home, &plugin, &events);
    // both forms of an MCP result are masked: Code Mode reuses the structured `output`
    let mcp = fired[0]["result"].to_string();
    assert_hidden(&mcp);
    assert!(fired[0]["result"]["output"]["email"].as_str().unwrap().starts_with("EMAIL_"), "{mcp}");
    assert_eq!(fired[0]["result"]["output"]["plan"], "pro");
    // OpenCode's own tools are left alone
    assert_eq!(fired[1]["result"], edit);

    // a token written by the model reaches the MCP server as the real value
    let token = fired[0]["result"]["output"]["email"].as_str().unwrap().to_string();
    let events = serde_json::json!([{ "when": "before", "tool": "crm_find_customer", "input": { "email": token } }]);
    let fired = opencode_tool_events(&home, &plugin, &events);
    assert_eq!(fired[0]["input"]["email"], "jean.dupont@example.com");

    // without the ranon binary, the MCP result is hidden rather than passed raw
    let text = std::fs::read_to_string(&plugin).unwrap();
    let broken = text
        .lines()
        .map(|line| if line.starts_with("const ranon = ") { "const ranon = \"/nonexistent/ranon\"" } else { line })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&plugin, broken).unwrap();
    let events = serde_json::json!([{ "when": "after", "tool": "crm_get_customer", "status": "completed", "input": {}, "result": result }]);
    let fired = opencode_tool_events(&home, &plugin, &events);
    let hidden = fired[0]["result"].to_string();
    assert_hidden(&hidden);
    assert!(hidden.contains("output hidden") && fired[0]["result"].get("output").is_none(), "{hidden}");
}

/// `program` from the PATH. Missing, the test is skipped, unless `env` is set (in CI) and then it fails.
fn required(program: &str, env: &str) -> Option<PathBuf> {
    let found = std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(program)).find(|path| path.is_file()));
    if found.is_none() {
        assert!(std::env::var_os(env).is_none(), "{program} is required when {env} is set");
        eprintln!("skipped: {program} not found");
    }
    found
}

// --- Codex CLI hook -----------------------------------------------------------------------------
// Payload shaped like the ones Codex 0.171.0 sent in a real session with a local MCP server (22/11/2026).

fn codex_hook(home: &Path, payload: &serde_json::Value) -> serde_json::Value {
    let mut child = command(BIN, home)
        .args(["hook", "codex"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn codex_mcp_results_come_back_masked_as_block_feedback() {
    let home = home();
    let customer =
        r#"{"id":42,"first_name":"Jean","last_name":"Dupont","email":"jean.dupont@example.com","plan":"pro"}"#;
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse", "tool_name": "mcp__crm__get_customer", "tool_use_id": "call_1",
        "tool_input": { "id": 42 }, "tool_response": { "content": [{ "type": "text", "text": customer }] },
    });
    let out = codex_hook(&home, &payload);
    assert_eq!(out["decision"], "block");
    let reason = out["reason"].as_str().unwrap();
    assert_hidden(reason);
    assert!(
        reason.contains("the call succeeded") && reason.contains("\"plan\":\"pro\"") && reason.contains("EMAIL_"),
        "{reason}"
    );

    // nothing to mask, or not an MCP tool: the result goes through untouched
    let plain = serde_json::json!({ "hook_event_name": "PostToolUse", "tool_name": "mcp__crm__ping",
        "tool_response": { "content": [{ "type": "text", "text": "pong" }] } });
    assert_eq!(codex_hook(&home, &plain), serde_json::json!({}));
    let shell =
        serde_json::json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_response": "Jean Dupont" });
    assert_eq!(codex_hook(&home, &shell), serde_json::json!({}));
}

#[test]
fn codex_hook_command_exits_2_without_ranon() {
    // the command `ranon init` writes, run by a shell that cannot find ranon: exit 2 makes Codex drop the result
    let home = home();
    let settings = stdout(
        command(BIN, &home).args(["init", "--agent", "codex", "--dry-run", "--shim-dir"]).arg(home.join("shims")),
    );
    let line = settings.lines().find(|line| line.contains("ranon hook codex")).expect(&settings);
    let hook = line.split("command = \"").nth(1).unwrap().split("\", timeout").next().unwrap();
    let out = Command::new("/bin/sh").args(["-c", hook]).env("PATH", "/usr/bin:/bin").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("output hidden"));
}

// --- Cursor -------------------------------------------------------------------------------------
// Cursor is not installed in CI: the payloads follow https://cursor.com/docs/agent/hooks (15/11/2026).

fn cursor_hook(home: &Path, payload: serde_json::Value) -> (serde_json::Value, i32) {
    let mut child = command(BIN, home)
        .args(["hook", "cursor"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), payload.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (json, out.status.code().unwrap_or(-1))
}

fn cursor_payload(event: &str, tool: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "conversation_id": "c", "generation_id": "g", "model": "m", "hook_event_name": event,
        "cursor_version": "1", "workspace_roots": ["/home/dev/project"], "tool_name": tool,
        "tool_input": input, "tool_use_id": "t", "cwd": "/home/dev/project" })
}

#[test]
fn cursor_init_writes_hooks_json() {
    let home = home();
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    stdout(command(BIN, &home).args(["init", "--agent", "cursor"]).current_dir(&project));
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".cursor/hooks.json")).unwrap()).unwrap();
    assert_eq!(config["version"], 1);
    for event in ["preToolUse", "postToolUse"] {
        assert_eq!(config["hooks"][event][0]["command"], "ranon hook cursor", "{config}");
    }
}

#[test]
fn cursor_shell_commands_run_through_ranon() {
    let home = home();
    let path = fake_psql(&home);
    let query = "psql -c 'select first_name, last_name, email from users'";
    let (output, code) =
        cursor_hook(&home, cursor_payload("preToolUse", "Shell", serde_json::json!({ "command": query })));
    assert_eq!(code, 0);
    let rewritten = output["updated_input"]["command"].as_str().unwrap();
    assert!(rewritten.starts_with("ranon sh -c "), "{output}");

    // Cursor runs the rewritten command in its shell, where `ranon` is on the PATH
    let bin = Path::new(BIN).parent().unwrap().display().to_string();
    let out = stdout(command("sh", &home).args(["-c", rewritten]).env("PATH", format!("{bin}:{path}")));
    assert_hidden(&out);
    assert!(out.contains("PERSON_") && out.contains("EMAIL_"), "{out}");

    let (output, _) =
        cursor_hook(&home, cursor_payload("preToolUse", "Shell", serde_json::json!({ "command": "git status" })));
    assert_eq!(output, serde_json::json!({}));
    let (output, _) =
        cursor_hook(&home, cursor_payload("preToolUse", "Shell", serde_json::json!({ "command": rewritten })));
    assert_eq!(output, serde_json::json!({}), "a rewritten command is not wrapped twice");
}

#[test]
fn cursor_mcp_results_are_masked() {
    let home = home();
    let result = serde_json::json!({ "content": [{ "type": "text", "text": "contact: jean.dupont@example.com" }] });
    let mut payload = cursor_payload("postToolUse", "MCP:notion-fetch", serde_json::json!({ "id": "page" }));
    payload["tool_output"] = result.to_string().into();
    let (output, code) = cursor_hook(&home, payload);
    assert_eq!(code, 0);
    let updated = &output["updated_mcp_tool_output"];
    assert_hidden(&updated.to_string());
    assert_eq!(updated["content"][0]["type"], "text");
    assert!(updated["content"][0]["text"].as_str().unwrap().contains("EMAIL_"), "{output}");
}

#[test]
fn cursor_mcp_inputs_get_real_values_back() {
    let home = home();
    let masked = stdout(
        command(BIN, &home)
            .args(["mask"])
            .stdin(std::fs::File::open(write(&home.join("in.txt"), "email: jean.dupont@example.com\n")).unwrap()),
    );
    let token = masked.trim().strip_prefix("email: ").unwrap().to_string();
    let payload = cursor_payload(
        "preToolUse",
        "MCP:notion-create-comment",
        serde_json::json!({ "text": format!("ask {token}") }),
    );
    let (output, _) = cursor_hook(&home, payload);
    assert_eq!(output["updated_input"]["text"], "ask jean.dupont@example.com");
}

#[test]
fn cursor_fails_closed() {
    let home = home();
    let (_, code) = cursor_hook(&home, serde_json::json!("not a payload"));
    assert_eq!(code, 2, "an unreadable payload blocks the call");

    // an unusable state hides an MCP result rather than letting it through
    write(&home.join("state/salt"), "x");
    std::fs::set_permissions(home.join("state"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut payload = cursor_payload("postToolUse", "MCP:notion-fetch", serde_json::json!({}));
    payload["tool_output"] = r#"{"email": "jean.dupont@example.com"}"#.into();
    let (output, code) = cursor_hook(&home, payload);
    std::fs::set_permissions(home.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code, 0);
    assert_hidden(&output.to_string());
    assert!(output.to_string().contains("output hidden"), "{output}");
}
