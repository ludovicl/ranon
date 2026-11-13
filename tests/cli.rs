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
        .env("RANON_CONFIG_DIR", home)
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
fn names_split_across_record_cells_are_masked() {
    let home = with_names("Jean\nDupont\nMarie\nMartin\nPaul\nDurand\nLucie\nPetit\n");
    let out =
        mask(&home, "EMAIL | Jean | Dupont | Acme\n[[\"Marie\", \"Martin\"], ['Paul', 'Durand']]\n42\tLucie\tPetit\n");
    assert_hidden(&out, &["Jean", "Dupont", "Marie", "Martin", "Paul", "Durand", "Lucie", "Petit"]);
    assert!(out.contains("| Acme") && out.starts_with("EMAIL | PERSON_"), "{out}");
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

// --- JSON documents and user rules ----------------------------------------------------------------

const STRIPE_CUSTOMER: &str = r#"{
  "id": "cus_NffrFeUfNV2Hib",
  "object": "customer",
  "name": "Jenny Rosen",
  "email": "jennyrosen@example.com",
  "phone": "+33612345678",
  "address": { "city": "Lyon", "country": "FR", "line1": "12 rue des Lilas", "postal_code": "69003" },
  "balance": 0,
  "currency": "eur",
  "invoice_settings": { "default_payment_method": "pm_1" },
  "metadata": {}
}"#;

#[test]
fn stripe_customer_json() {
    let out = mask(&home(), STRIPE_CUSTOMER);
    assert_hidden(&out, &["Jenny", "Rosen", "jennyrosen@", "612345678", "Lyon", "Lilas", "69003"]);
    for kept in ["cus_NffrFeUfNV2Hib", "\"currency\": \"eur\"", "pm_1", "\"balance\": 0"] {
        assert!(out.contains(kept), "{kept} missing in:\n{out}");
    }
}

#[test]
fn stripe_payment_method_and_connect_person() {
    let pm = r#"{"object":"payment_method","billing_details":{"name":"Jenny Rosen","email":null},"card":{"brand":"visa","last4":"4242"}}"#;
    let out = mask(&home(), pm);
    assert_hidden(&out, &["Jenny"]);
    assert!(out.contains(r#""last4":"4242""#) && out.contains(r#""email":null"#), "{out}");

    let person =
        r#"{"object":"person","first_name":"Jenny","dob":{"day":12,"month":3,"year":1985},"id_number_provided":true}"#;
    let out = mask(&home(), person);
    assert_hidden(&out, &["Jenny", "1985", r#""day":12"#]);
    assert!(out.contains(r#""id_number_provided":true"#), "{out}");
}

#[test]
fn hook_masks_stripe_cli_and_structured_mcp_responses() {
    let updated = hook(&home(), bash("stripe customers retrieve cus_1", STRIPE_CUSTOMER)).unwrap();
    assert_hidden(&updated.to_string(), &["Jenny", "Lilas"]);

    let payload = serde_json::json!({ "tool_name": "mcp__notion__query", "tool_input": {},
        "tool_response": { "results": [{ "object": "page", "properties": { "Email": { "type": "email", "email": "a@b.fr" } } }] } });
    let updated = hook(&home(), payload).unwrap();
    assert_hidden(&updated.to_string(), &["a@b.fr"]);
}

#[test]
fn user_rules_file_extends_builtins() {
    let home = home();
    std::fs::write(
        home.join("rules.toml"),
        r#"
keep = ["^city$"]
[[column]]
pattern = "^(nom|prénom)$"
kind = "PERSON"
[[value]]
pattern = '\bPI-\d{4}\b'
kind = "TICKET"
"#,
    )
    .unwrap();
    let out = mask(&home, "id\tnom\tcity\n1\tDupont\tLyon\n\nvoir PI-1234\n");
    assert_hidden(&out, &["Dupont", "PI-1234"]);
    assert!(out.contains("Lyon") && out.contains("TICKET_"), "{out}");

    let (listing, _, code) = run_with(Path::new(BIN), &home, &["rules", "prénom", "status"], "");
    assert_eq!(code, 0);
    assert!(listing.contains("prénom: PERSON") && listing.contains("status: kept visible"), "{listing}");
}

#[test]
fn invalid_rules_file_fails_closed() {
    let home = home();
    std::fs::write(home.join("rules.toml"), "[[column]]\npattern = \"(\"\nkind = \"PERSON\"\n").unwrap();
    let (out, err, code) = run_with(Path::new(BIN), &home, &["mask"], "email\njean@x.fr\n");
    assert_ne!(code, 0);
    assert!(out.is_empty() && err.contains("invalid pattern"), "{out}{err}");

    let updated = hook(&home, bash("psql -c q", "email\njean@x.fr")).unwrap();
    assert!(updated["stdout"].as_str().unwrap().contains("masking failed"));
}

#[test]
fn notion_people_and_users_are_masked() {
    let page = r#"{"object":"page","properties":{
  "Assignee":{"type":"people","people":[{"object":"user","id":"u1","name":"Jean Dupont","type":"person","person":{"email":"jean@example.com"}}]},
  "Status":{"type":"status","status":{"name":"In progress"}},
  "Name":{"type":"title","title":[{"plain_text":"Refund order 42"}]}}}"#;
    let out = mask(&home(), page);
    assert_hidden(&out, &["Jean Dupont", "jean@example.com"]);
    for kept in ["In progress", "Refund order 42", "\"id\": \"u1\""] {
        assert!(out.contains(kept), "{kept} missing in:\n{out}");
    }
}

#[test]
fn rules_d_files_are_loaded_and_named() {
    let home = home();
    std::fs::create_dir_all(home.join("rules.d")).unwrap();
    std::fs::write(home.join("rules.d/notion.toml"), "[[column]]\npattern = '^client$'\nkind = \"PERSON\"\n").unwrap();
    std::fs::write(home.join("rules.d/leeto.toml"), "keep = ['^city$']\ncommands = ['composio']\n").unwrap();
    let (listing, _, code) = run_with(Path::new(BIN), &home, &["rules", "Client", "city", "iban", "status"], "");
    assert_eq!(code, 0);
    for line in [
        "Client: PERSON (notion.toml)",
        "city: kept visible (keep in leeto.toml)",
        "iban: IBAN (built-in payments)",
        "status: kept visible",
        "commands: composio",
    ] {
        assert!(listing.contains(line), "{line} missing in:\n{listing}");
    }
    let updated = hook(&home, bash("composio execute GMAIL_FETCH_EMAILS", "From: paul@example.com")).unwrap();
    assert_hidden(&updated.to_string(), &["paul@example.com"]);
}

fn pre_hook(home: &Path, tool_input: serde_json::Value) -> Option<serde_json::Value> {
    let payload = serde_json::json!({ "hook_event_name": "PreToolUse", "tool_name": "mcp__notion__notion-create-comment",
        "tool_input": tool_input });
    let out = ranon(home, &["hook", "claude"], &payload.to_string());
    (!out.trim().is_empty())
        .then(|| serde_json::from_str::<serde_json::Value>(&out).unwrap()["hookSpecificOutput"].clone())
}

#[test]
fn pre_tool_use_restores_real_values_and_asks() {
    let home = home();
    let masked = mask(&home, "email\njean@example.com\n");
    let token = masked.lines().nth(1).unwrap().trim().to_string();
    assert!(token.starts_with("EMAIL_"), "{masked}");

    let output = pre_hook(&home, serde_json::json!({ "page_id": "p1", "rich_text": [{ "text": { "content": format!("Contact {token}, unknown PERSON_000000") } }] })).unwrap();
    assert_eq!(output["permissionDecision"], "ask");
    assert_eq!(output["updatedInput"]["page_id"], "p1");
    assert_eq!(
        output["updatedInput"]["rich_text"][0]["text"]["content"],
        "Contact jean@example.com, unknown PERSON_000000"
    );

    assert!(pre_hook(&home, serde_json::json!({ "page_id": "p1", "content": "no token here" })).is_none());
}

// --- formats that used to leak -------------------------------------------------------------------

#[test]
fn camel_case_keys_are_masked() {
    let out = mask(
        &home(),
        r#"{"dateOfBirth":"1990-01-01","billingAddress":{"line1":"1 rue X"},"socialSecurityNumber":"190017500000000","userEmail":"zoe@example.com","userStatus":"active"}"#,
    );
    assert_hidden(&out, &["1990-01-01", "1 rue X", "190017500000000", "zoe@example.com"]);
    assert!(out.contains(r#""userStatus":"active""#), "{out}");
}

#[test]
fn slack_user_columns_and_objects_are_masked() {
    let out = mask(&home(), "msgID,userID,userName,realName,text\n1,U1,zoe.xyz,Zoe Xyz,hello\n");
    assert_hidden(&out, &["zoe.xyz", "Zoe Xyz"]);
    assert!(out.contains("1,U1,PERSON_") && out.contains(",hello"), "{out}");

    let out = mask(&home(), r#"{"id":"U1","real_name":"Zoe Xyz","profile":{"display_name":"zoe"}}"#);
    assert_hidden(&out, &["Zoe Xyz", "zoe\""]);
}

#[test]
fn json_after_a_banner_and_json_lines_are_masked_by_key_path() {
    let banner =
        "Running rails runner on app... up, run.1234\n{\"customer\":{\"name\":\"Zoe Xyz\",\"plan\":\"pro\"}}\n";
    let out = mask(&home(), banner);
    assert_hidden(&out, &["Zoe Xyz"]);
    assert!(out.starts_with("Running rails runner on app... up, run.1234\n"), "{out}");
    assert!(out.contains(r#""plan":"pro""#), "{out}");

    let lines = "{\"object\":\"customer\",\"name\":\"Zoe Xyz\"}\n{\"object\":\"customer\",\"name\":\"Ana Abc\"}\n";
    let out = mask(&home(), lines);
    assert_hidden(&out, &["Zoe Xyz", "Ana Abc"]);
    assert_eq!(out.lines().count(), 2, "{out}");

    let pretty = "done\n{\n  \"customer\": {\n    \"name\": \"Zoe Xyz\"\n  }\n}\nbye\n";
    let out = mask(&home(), pretty);
    assert_hidden(&out, &["Zoe Xyz"]);
    assert!(out.starts_with("done\n{\n") && out.ends_with("}\nbye\n"), "{out}");
}

#[test]
fn text_that_only_looks_like_json_is_left_alone() {
    let text = "[1] id=1 status=ok\n{Lyon,Paris}\n[link](https://example.org)\n";
    assert_eq!(mask(&home(), text), text);
}

#[test]
fn json_in_a_csv_cell_is_masked() {
    let out = mask(&home(), "id,metadata\n1,\"{\"\"first_name\"\": \"\"Zoe\"\", \"\"plan\"\": \"\"pro\"\"}\"\n");
    assert_hidden(&out, &["Zoe"]);
    assert!(out.contains("\"\"plan\"\": \"\"pro\"\""), "{out}");
}

#[test]
fn escaped_quotes_do_not_cut_a_value_short() {
    let out = mask(&home(), "#<User first_name: \"Zoe \\\"Zozo\\\" Xyz\", state: \"active\">\n");
    assert_hidden(&out, &["Zoe", "Zozo", "Xyz"]);
    assert!(out.contains("state: \"active\""), "{out}");
}

#[test]
fn yaml_like_lines_are_masked_by_key() {
    let out = mask(&home(), "id: 12\nfirst_name: Zoe\n- last_name: Xyz\nstatus: active\nbirth_date: nil\n");
    assert_hidden(&out, &["Zoe", "Xyz"]);
    assert!(out.contains("id: 12\n") && out.contains("status: active\n") && out.contains("birth_date: nil\n"), "{out}");
}

#[test]
fn identifiers_and_references_are_not_taken_for_contact_data() {
    let text = "uuid 3f2a-0123-4567-89ab, order AB12CDEF3456, sku XY99 ABCD EFGH IJKL\n";
    assert_eq!(mask(&home(), text), text);
    let table =
        " id | address_id | phone_verified_at\n----+------------+-------------------\n  1 |         42 | 2026-01-01\n";
    assert_eq!(mask(&home(), table), table);
}

#[test]
fn spellings_of_one_email_share_a_token() {
    let out = mask(&home(), "from Zoe@Example.com to zoe@example.com\n");
    let tokens: Vec<&str> = out.split_whitespace().filter(|word| word.starts_with("EMAIL_")).collect();
    assert_eq!(tokens.len(), 2, "{out}");
    assert_eq!(tokens[0], tokens[1]);
}

#[test]
fn csv_record_with_line_breaks_stays_one_record() {
    let csv = "MsgID,RealName,Text,Email\n1,Zoe Xyz,\"first line\n\nthird, \"\"quoted\"\" line\",zoe@x.fr\n2,Ana Abc,short,ana@x.fr\n";
    let out = mask(&home(), csv);
    assert_hidden(&out, &["Zoe Xyz", "Ana Abc", "zoe@x.fr", "ana@x.fr", "row hidden"]);
    assert!(out.contains("\"first line\n\nthird, \"\"quoted\"\" line\",EMAIL_"), "{out}");
    assert!(out.contains("\n2,PERSON_"), "{out}");
}

#[test]
fn chained_rtk_results_are_each_masked() {
    let out = mask(&home(), "id\tfirst_name\n1\tZoe\n[1] id=1 first_name=Zoe status=ok\n");
    assert_hidden(&out, &["Zoe", "row hidden"]);
    assert!(out.ends_with("status=ok\n"), "{out}");
}

#[test]
fn name_beside_an_iban_is_the_account_holder() {
    let home = home();
    let json = mask(&home, r#"{"id":5,"bic":"BNPAFRPPXXX","iban":"FR7630006000011234567890189","name":"JEAN DUPONT"}"#);
    assert_hidden(&json, &["JEAN DUPONT", "FR7630006000011234567890189"]);
    assert!(json.contains("BNPAFRPPXXX"), "{json}");
    let table = mask(&home, "id | name | iban\n----+------+------\n 5 | Dupont SARL | FR7630006000011234567890189\n");
    assert_hidden(&table, &["Dupont SARL"]);
}

#[test]
fn name_without_an_iban_stays_visible() {
    let out = mask(&home(), "id | name | state\n----+------+------\n 5 | Ticket cinema | active\n");
    assert!(out.contains("Ticket cinema"), "{out}");
    let json = mask(&home(), r#"{"id":5,"name":"Ticket cinema","state":"active"}"#);
    assert!(json.contains("Ticket cinema"), "{json}");
}

#[test]
fn json_opened_at_the_end_of_a_line_is_masked_by_key() {
    let inspect = "#<Treezor::Beneficiary:0x16d78 id=5> JSON: {\n  \"id\": 5,\n  \"iban\": \"FR7630006000011234567890189\",\n  \"name\": \"JEAN DUPONT\"\n}\n=> nil\n";
    let out = mask(&home(), inspect);
    assert_hidden(&out, &["JEAN DUPONT", "FR7630006000011234567890189"]);
    assert!(out.starts_with("#<Treezor::Beneficiary:0x16d78 id=5> JSON: {"), "{out}");
    assert!(out.ends_with("}\n=> nil\n"), "{out}");
    let code = "fn main() {\n    let name = 1;\n}\n";
    assert_eq!(mask(&home(), code), code);
}

// --- fail closed, doctor, forget ----------------------------------------------------------------

#[test]
fn hook_hides_output_when_the_payload_is_unreadable() {
    let out = ranon(&home(), &["hook", "claude"], r#"{"tool_name":"Bash", truncated"#);
    let updated =
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["hookSpecificOutput"]["updatedToolOutput"].clone();
    assert!(updated["stdout"].as_str().unwrap().contains("output hidden"), "{out}");

    let out = ranon(&home(), &["hook", "claude"], "not json");
    assert!(out.contains("output hidden") && out.contains(r#""type":"text""#), "{out}");
}

#[test]
fn hook_fails_closed_on_an_empty_salt() {
    let home = home();
    std::fs::write(home.join("salt"), "").unwrap();
    let updated = hook(&home, bash("psql -c q", "first_name\tid\nJean\t1")).unwrap();
    let stdout = updated["stdout"].as_str().unwrap();
    assert!(stdout.contains("output hidden"), "{stdout}");
    assert_hidden(stdout, &["Jean"]);
}

#[test]
fn hook_logs_what_it_did_without_content() {
    let home = home();
    hook(&home, bash("psql -c q", "first_name\temail\nJean\tjean@example.com")).unwrap();
    hook(&home, bash("psql -At -c q", "Jean")).unwrap();
    assert!(hook(&home, bash("git status", "clean")).is_none());
    let log = std::fs::read_to_string(home.join("hook.log")).unwrap();
    let entries: Vec<Vec<&str>> = log.lines().map(|line| line.split('\t').skip(1).collect()).collect();
    assert_eq!(entries, [["Bash", "masked", "2"], ["Bash", "hidden", "0"]]);
    assert_hidden(&log, &["Jean", "psql"]);
}

/// A state directory created the way ranon creates it, unlike the world-readable `home()`.
fn private_home() -> PathBuf {
    let home = home();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}

#[test]
fn doctor_reports_the_setup() {
    let home = private_home();
    ranon(&home, &["dict", "build", "--stdin"], "Jean Dupont\n");
    let (out, _, _) = run_with(Path::new(BIN), &home, &["doctor"], "");
    for line in ["ok    rules: loaded", "is private", "ok    name dictionary: 2 hashed words", "ok    self-test"] {
        assert!(out.contains(line), "{line} missing in:\n{out}");
    }

    std::fs::write(home.join("rules.toml"), "colum = []").unwrap();
    let (out, _, code) = run_with(Path::new(BIN), &home, &["doctor"], "");
    assert_eq!(code, 1);
    assert!(out.contains("FAIL  rules:"), "{out}");
}

#[test]
fn doctor_flags_state_readable_by_others() {
    let home = private_home();
    mask(&home, "email\njean@x.fr\n");
    std::fs::set_permissions(home.join("map.tsv"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let (out, _, code) = run_with(Path::new(BIN), &home, &["doctor"], "");
    assert_eq!(code, 1);
    let exposed = format!("chmod go-rwx on {}\n", home.join("map.tsv").display());
    assert!(out.contains("FAIL  state: readable by other users") && out.contains(&exposed), "{out}");
}

#[test]
fn forget_drops_real_values_but_tokens_stay_stable() {
    let home = home();
    let masked = mask(&home, "email\njean@example.com\n");
    assert_eq!(ranon(&home, &["forget", "--older-than", "1"], "").trim(), "0 value(s) forgotten, 1 kept");
    assert_eq!(ranon(&home, &["unmask"], &masked), "email\njean@example.com\n");

    assert_eq!(ranon(&home, &["forget"], "").trim(), "1 value(s) forgotten, 0 kept");
    assert_eq!(ranon(&home, &["unmask"], &masked), masked, "a forgotten token cannot be unmasked");
    assert_eq!(mask(&home, "email\njean@example.com\n"), masked, "same value, same token");
    assert_eq!(std::fs::metadata(home.join("map.tsv")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn token_maps_written_before_timestamps_still_unmask() {
    let home = home();
    mask(&home, "email\nx@example.com\n");
    std::fs::write(home.join("map.tsv"), "PERSON_abc123\tJean\n").unwrap();
    assert_eq!(ranon(&home, &["unmask"], "PERSON_abc123"), "Jean");
    assert_eq!(ranon(&home, &["forget", "--older-than", "1"], "").trim(), "1 value(s) forgotten, 0 kept");
}
