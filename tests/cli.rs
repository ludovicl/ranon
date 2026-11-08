//! End-to-end tests: run the real binary with an isolated state directory.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_ranon");

fn home() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ranon-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_with(program: &Path, home: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut child = Command::new(program)
        .args(args)
        .env("RANON_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap(), out.status.code().unwrap_or(-1))
}

fn ranon(home: &Path, args: &[&str], stdin: &str) -> String {
    run_with(Path::new(BIN), home, args, stdin).0
}

fn mask(home: &Path, text: &str) -> String {
    ranon(home, &["mask"], text)
}

fn with_names(names: &str) -> PathBuf {
    let home = home();
    ranon(&home, &["dict", "build", "--stdin"], names);
    home
}

fn assert_hidden(output: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(!output.contains(secret), "`{secret}` leaked in:\n{output}");
    }
}

// --- tables -------------------------------------------------------------------------------------

#[test]
fn psql_aligned_table() {
    let out = mask(
        &home(),
        " id | first_name |     email      | city\n----+------------+----------------+------\n  1 | Jean       | jean@example.com | Lyon\n(1 row)\n",
    );
    assert_hidden(&out, &["Jean", "jean@example.com", "Lyon"]);
    assert!(out.contains("PERSON_") && out.contains("EMAIL_") && out.contains("ADDRESS_"));
    assert!(out.contains("(1 row)"), "footer must stay: {out}");
}

#[test]
fn psql_single_column_table_keeps_footer() {
    let out = mask(&home(), " first_name \n------------\n Jean\n(1 row)\n");
    assert_hidden(&out, &["Jean"]);
    assert!(out.contains("(1 row)"));
}

#[test]
fn psql_expanded_records() {
    let out = mask(
        &home(),
        "-[ RECORD 1 ]-------\nid         | 7\nfirst_name | Jean\nlast_name  | Dupont\nstatus     | active\n",
    );
    assert_hidden(&out, &["Jean", "Dupont"]);
    assert!(out.contains("status     | active") && out.contains("id         | 7"));
}

#[test]
fn psql_unaligned_and_csv() {
    let unaligned = mask(&home(), "first_name|organisation_name|id\nJean|Acme|1\n(1 row)\n");
    assert_hidden(&unaligned, &["Jean", "Acme"]);
    let csv = mask(&home(), "first_name,organisation_name,id\nJean,\"Acme, Inc\",1\n");
    assert_hidden(&csv, &["Jean", "Acme"]);
    assert!(csv.lines().nth(1).unwrap().ends_with(",1"), "{csv}");
}

#[test]
fn rtk_formats() {
    let tsv = mask(&home(), "first_name\temail\tid\nJean\tjean@x.fr\t42\nMarie\t\t43\n");
    assert_hidden(&tsv, &["Jean", "Marie", "jean@x.fr"]);
    assert!(tsv.contains("\t42") && tsv.contains("\t\t43"), "{tsv}");
    let expanded = mask(&home(), "[1] first_name=Jean last_name=Dupont city=Paris status=ok\n");
    assert_hidden(&expanded, &["Jean", "Dupont", "Paris"]);
    assert!(expanded.contains("status=ok"));
}

#[test]
fn ambiguous_row_is_hidden_not_leaked() {
    let out = mask(&home(), "first_name|id\nJe|an|1\n");
    assert_hidden(&out, &["Je|an"]);
    assert!(out.contains("row hidden"));
}

#[test]
fn null_values_stay_null() {
    let out = mask(&home(), "first_name\temail\nNULL\t\n");
    assert!(out.contains("NULL"));
}

// --- key/value and patterns ---------------------------------------------------------------------

#[test]
fn treezor_json_and_rails_inspect() {
    let treezor = r#"#<Treezor::User:0x1 id=12> JSON: {
  "userId": 12, "userStatus": "VALIDATED", "kycLevel": 2,
  "firstname": "Jean", "lastname": "Dupont", "birthday": "1985-03-12", "placeOfBirth": "Lyon",
  "nationality": "FR", "email": "jean@example.com", "mobile": "0612345678", "address1": "12 rue des Lilas",
  "postcode": "69003", "city": "Lyon", "country": "FR", "taxNumber": "1234567890123"
}"#;
    let out = mask(&home(), treezor);
    assert_hidden(
        &out,
        &["Jean", "Dupont", "1985-03-12", "Lyon", "jean@example.com", "0612345678", "Lilas", "69003", "1234567890123"],
    );
    assert!(out.contains(r#""userStatus": "VALIDATED""#) && out.contains(r#""country": "FR""#));

    let rails = r#"#<User id: 3, first_name: "Jean", last_name: "Dupont", email: "j@x.fr", state: "active">"#;
    let out = mask(&home(), rails);
    assert_hidden(&out, &["Jean", "Dupont", "j@x.fr"]);
    assert!(out.contains(r#"state: "active""#));

    let hash = r#"{"last_name"=>"Dupont", "iban"=>"FR7630006000011234567890189"}"#;
    assert_hidden(&mask(&home(), hash), &["Dupont", "FR763000"]);
}

#[test]
fn contact_patterns_in_free_text() {
    let out = mask(
        &home(),
        "écrire à paul@example.com ou au 06 12 34 56 78, IBAN FR76 3000 6000 0112 3456 7890 189, montant 1234.56",
    );
    assert_hidden(&out, &["paul@example.com", "06 12 34 56 78", "FR76 3000"]);
    assert!(out.contains("1234.56"));
}

// --- names dictionary ---------------------------------------------------------------------------

#[test]
fn full_names_are_masked_lone_words_are_not() {
    let home = with_names("Jean\nDupont\nMarie-Anne\nBonjour\nParis\nDurand\nMartin\n");
    let out = mask(
        &home,
        "Bonjour, Jean Dupont et Marie-Anne Martin habitent Paris. DUPONT Jean aussi. Contact : M. Durand. Martin seul.",
    );
    assert_hidden(&out, &["Jean", "Dupont", "DUPONT", "Marie", "Anne", "Durand"]);
    assert!(out.starts_with("Bonjour,"), "lone dictionary word must stay: {out}");
    assert!(out.contains("habitent Paris") && out.contains("Martin seul"), "{out}");
    assert!(out.contains("M. PERSON_"), "title must stay: {out}");
}

#[test]
fn lowercase_words_and_titles_are_not_names() {
    let home = with_names("Rose\nPetit\nMme\n");
    let out = mask(&home, "la rose petit et Mme Petit");
    assert!(out.starts_with("la rose petit et Mme PERSON_"), "{out}");
}

#[test]
fn dictionary_stores_hashes_only() {
    let home = with_names("Dupont\n");
    let bytes = std::fs::read(home.join("names.bin")).unwrap();
    assert_eq!(bytes.len(), 8);
    assert!(!String::from_utf8_lossy(&bytes).to_lowercase().contains("dupont"));
}

// --- tokens and state ---------------------------------------------------------------------------

#[test]
fn tokens_are_stable_idempotent_and_reversible() {
    let home = home();
    let first = mask(&home, "first_name\tid\nJean\t1\n");
    let second = mask(&home, "first_name\tid\nJean\t2\n");
    let token = |text: &str| text.split_whitespace().find(|w| w.starts_with("PERSON_")).unwrap().to_string();
    assert_eq!(token(&first), token(&second));
    assert_eq!(mask(&home, &first), first, "masking twice must not change tokens");
    assert_eq!(ranon(&home, &["unmask"], &first), "first_name\tid\nJean\t1\n");
}

#[test]
fn different_salts_give_different_tokens() {
    let a = mask(&home(), "email\njean@x.fr\n");
    let b = mask(&home(), "email\njean@x.fr\n");
    assert_ne!(a, b);
}

#[test]
fn state_files_are_private() {
    let home = home();
    mask(&home, "email\njean@x.fr\n");
    for file in ["salt", "map.tsv"] {
        let mode = std::fs::metadata(home.join(file)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{file}");
    }
}

// --- Claude Code hook ---------------------------------------------------------------------------

fn hook(home: &Path, payload: serde_json::Value) -> Option<serde_json::Value> {
    let out = ranon(home, &["hook", "claude"], &payload.to_string());
    (!out.trim().is_empty()).then(|| {
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["hookSpecificOutput"]["updatedToolOutput"].clone()
    })
}

fn bash(command: &str, stdout: &str) -> serde_json::Value {
    serde_json::json!({ "tool_name": "Bash", "tool_input": { "command": command },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false } })
}

#[test]
fn hook_masks_psql_output_and_keeps_shape() {
    let updated = hook(&home(), bash("psql -c q", "first_name\tid\nJean\t1")).unwrap();
    assert!(updated["stdout"].as_str().unwrap().starts_with("first_name\tid\nPERSON_"));
    assert_eq!(updated["interrupted"], false);
    assert_eq!(updated["isImage"], false);
}

#[test]
fn hook_ignores_other_commands() {
    assert!(hook(&home(), bash("git log", "Jean <jean@x.fr>")).is_none());
    assert!(hook(&home(), bash("cat privacy-psql.py", "jean@x.fr")).is_none());
}

#[test]
fn hook_hides_tuples_only_output() {
    let updated = hook(&home(), bash("psql -At -c q", "Jean|Acme")).unwrap();
    assert_hidden(updated["stdout"].as_str().unwrap(), &["Jean", "Acme"]);
}

#[test]
fn hook_masks_heroku_console_and_mcp_responses() {
    let updated = hook(&home(), bash("heroku run 'rails c --sandbox' -a app", r#"first_name: "Jean""#)).unwrap();
    assert_hidden(&updated.to_string(), &["Jean"]);
    let mcp = serde_json::json!({ "tool_name": "mcp__notion__fetch", "tool_input": {},
        "tool_response": [{ "type": "text", "text": "contact: jean@example.com" }] });
    let updated = hook(&home(), mcp).unwrap();
    assert_hidden(&updated.to_string(), &["jean@example.com"]);
    assert_eq!(updated[0]["type"], "text");
}

#[test]
fn hook_fails_closed_when_state_is_unusable() {
    let blocker = home().join("not-a-dir");
    std::fs::write(&blocker, "").unwrap();
    let updated = hook(&blocker.join("state"), bash("psql -c q", "first_name\nJean\n------")).unwrap();
    let stdout = updated["stdout"].as_str().unwrap();
    assert!(stdout.contains("output hidden"), "{stdout}");
    assert_hidden(stdout, &["Jean"]);
}

// --- proxy and shim -----------------------------------------------------------------------------

#[test]
fn proxy_masks_output_and_keeps_exit_code() {
    let (out, _, code) =
        run_with(Path::new(BIN), &home(), &["sh", "-c", "printf 'first_name\\tid\\nJean\\t1\\n'; exit 3"], "");
    assert_hidden(&out, &["Jean"]);
    assert!(out.contains("PERSON_"));
    assert_eq!(code, 3);
}

#[test]
fn shim_runs_the_real_command_masked() {
    let home = home();
    let shims = home.join("shims");
    ranon(&home, &["init", "--shim-dir", shims.to_str().unwrap(), "printf"], "");
    let (out, _, code) = run_with(&shims.join("printf"), &home, &["email\\njean@x.fr\\n"], "");
    assert_eq!(code, 0);
    assert_hidden(&out, &["jean@x.fr"]);
    assert!(out.contains("EMAIL_"));
}

#[test]
fn proxy_reports_missing_command() {
    let (_, err, code) = run_with(Path::new(BIN), &home(), &["definitely-not-a-command"], "");
    assert_eq!(code, 127);
    assert!(err.contains("no real"));
}
