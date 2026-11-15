//! Each agent, simulated as closely as the CI allows without calling a model:
//! - Claude Code: payloads captured from a real session (synthetic data, ids and paths replaced) go
//!   through the hook, so a change in their shape shows up here.
//! - Codex CLI and OpenCode: `ranon init` writes their config, the test runs a shell the way the agent
//!   would with that config, and a fake `psql` behind the shims must come out masked.

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

#[test]
fn codex_shell_commands_are_masked() {
    let home = home();
    let path = fake_psql(&home);
    let shims = home.join("shims");
    stdout(
        command(BIN, &home)
            .args(["init", "-g", "--agent", "codex", "--shim-dir"])
            .arg(&shims)
            .arg("psql")
            .env("PATH", &path),
    );

    // Codex replaces the PATH of its shell commands with this value
    let config: toml_edit::DocumentMut =
        std::fs::read_to_string(home.join(".codex/config.toml")).unwrap().parse().unwrap();
    let codex_path = config["shell_environment_policy"]["set"]["PATH"].as_str().unwrap().to_string();
    assert_hidden(&psql_through(&home, &codex_path));
}

#[test]
fn opencode_shell_commands_are_masked() {
    let Ok(node) = which("node") else {
        assert!(std::env::var_os("CI").is_none(), "node is required in CI to run the OpenCode plugin");
        eprintln!("skipped: node not found");
        return;
    };
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

fn which(program: &str) -> Result<PathBuf, ()> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(program)).find(|path| path.is_file()))
        .ok_or(())
}
