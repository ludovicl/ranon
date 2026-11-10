//! What counts as personal data: sensitive column/key names, value patterns and the commands to mask.
//!
//! Built-in rules live in `builtin.rs`, grouped by source. Users add their own in `rules.toml` and
//! `rules.d/*.toml` under the config directory (see `config_dir`), one file per source if they like.
//! Every rule remembers where it comes from, so `ranon rules <name>` can tell which one applies.

mod builtin;

use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    column: Vec<RuleConfig>,
    #[serde(default)]
    value: Vec<RuleConfig>,
    #[serde(default)]
    keep: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleConfig {
    pattern: String,
    kind: String,
}

#[derive(Debug)]
struct Rule {
    re: Regex,
    kind: &'static str,
    source: &'static str,
}

pub struct Rules {
    keep: Vec<Rule>,
    columns: Vec<Rule>,
    values: Vec<(Regex, &'static str)>,
    commands: Regex,
    command_list: Vec<(String, &'static str)>,
    files: Vec<PathBuf>,
    sources: Vec<&'static str>,
}

static RULES: OnceLock<Rules> = OnceLock::new();

/// `$RANON_CONFIG_DIR`, else `$XDG_CONFIG_HOME/ranon`, else `~/.config/ranon`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RANON_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("ranon")
}

/// `rules.toml`, then `rules.d/*.toml` in name order.
fn config_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    if dir.join("rules.toml").is_file() {
        files.push(dir.join("rules.toml"));
    }
    match std::fs::read_dir(dir.join("rules.d")) {
        Ok(entries) => {
            let mut extra: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
                .collect();
            extra.sort();
            files.extend(extra);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", dir.join("rules.d").display())),
    }
    Ok(files)
}

/// Loads built-in rules plus the user files. An invalid file is an error: masking must not silently degrade.
pub fn load() -> Result<(), String> {
    if RULES.get().is_some() {
        return Ok(());
    }
    let mut configs = Vec::new();
    for path in config_files(&config_dir())? {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        configs.push((path, config));
    }
    let rules = build(configs)?;
    RULES.get_or_init(|| rules);
    Ok(())
}

fn rules() -> &'static Rules {
    RULES.get_or_init(|| build(Vec::new()).expect("built-in rules are valid"))
}

fn compile(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|e| format!("invalid pattern `{pattern}`: {e}"))
}

/// Strings built from config files live for the whole process.
fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn user_rules(list: Vec<RuleConfig>, source: &'static str) -> Result<Vec<Rule>, String> {
    list.into_iter()
        .map(|rule| {
            if rule.kind.is_empty() || !rule.kind.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(format!("{source}: invalid kind `{}`, use uppercase letters only (PERSON)", rule.kind));
            }
            let re = compile(&rule.pattern).map_err(|e| format!("{source}: {e}"))?;
            Ok(Rule { re, kind: leak(rule.kind), source })
        })
        .collect()
}

fn build(configs: Vec<(PathBuf, Config)>) -> Result<Rules, String> {
    let mut rules = Rules {
        keep: Vec::new(),
        columns: Vec::new(),
        values: Vec::new(),
        commands: Regex::new("$^").unwrap(),
        command_list: Vec::new(),
        files: Vec::new(),
        sources: Vec::new(),
    };
    // user rules come first so they can override a built-in kind
    for (path, config) in configs {
        let source = leak(path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()));
        for pattern in &config.keep {
            let re = compile(pattern).map_err(|e| format!("{source}: {e}"))?;
            rules.keep.push(Rule { re, kind: "", source });
        }
        rules.columns.extend(user_rules(config.column, source)?);
        rules.values.extend(user_rules(config.value, source)?.into_iter().map(|rule| (rule.re, rule.kind)));
        rules.command_list.extend(config.commands.into_iter().map(|command| (command, source)));
        rules.files.push(path);
        rules.sources.push(source);
    }
    for set in builtin::SETS {
        let source = leak(format!("built-in {}", set.name));
        rules.sources.push(source);
        for (pattern, kind) in set.columns {
            rules.columns.push(Rule { re: Regex::new(pattern).unwrap(), kind, source });
        }
        rules.values.extend(set.values.iter().map(|(pattern, kind)| (Regex::new(pattern).unwrap(), *kind)));
        rules.command_list.extend(set.commands.iter().map(|command| (command.to_string(), source)));
    }
    let alternatives: Vec<String> = rules
        .command_list
        .iter()
        .map(|(command, _)| command.split_whitespace().map(regex::escape).collect::<Vec<_>>().join(r"\s+"))
        .collect();
    rules.commands = compile(&format!(r"(^|[\s;&|(/])({})(\s|$)", alternatives.join("|")))?;
    Ok(rules)
}

/// What applies to a name: `Ok(rule)` when masked, `Err(Some(keep rule))` when explicitly kept visible.
fn lookup<'a>(rules: &'a Rules, name: &str) -> Result<&'a Rule, Option<&'a Rule>> {
    let name = name.trim().trim_matches(['"', ':']).to_lowercase();
    if let Some(keep) = rules.keep.iter().find(|rule| rule.re.is_match(&name)) {
        return Err(Some(keep));
    }
    rules.columns.iter().find(|rule| rule.re.is_match(&name)).ok_or(None)
}

impl Rules {
    fn kind(&self, name: &str) -> Option<&'static str> {
        lookup(self, name).ok().map(|rule| rule.kind)
    }
}

/// Kind of a column or key name (`first_name`, `"email"`, `billing_details.name`), if sensitive.
pub fn column_kind(name: &str) -> Option<&'static str> {
    rules().kind(name)
}

/// Which rule applies to a name, and where it comes from.
pub fn explain(name: &str) -> String {
    match lookup(rules(), name) {
        Ok(rule) => format!("{} ({})", rule.kind, rule.source),
        Err(Some(keep)) => format!("kept visible (keep in {})", keep.source),
        Err(None) => "kept visible".to_string(),
    }
}

pub fn value_patterns() -> &'static [(Regex, &'static str)] {
    &rules().values
}

/// Whether a shell command runs one of the commands whose output must be masked.
pub fn masks_command(command: &str) -> bool {
    rules().commands.is_match(command)
}

pub fn summary() -> String {
    let rules = rules();
    let mut lines = vec![format!("config directory: {}", config_dir().display())];
    if rules.files.is_empty() {
        lines.push("user files: none (rules.toml, rules.d/*.toml)".to_string());
    }
    lines.extend(rules.files.iter().map(|path| format!("user file: {}", path.display())));
    for source in &rules.sources {
        let columns = rules.columns.iter().filter(|rule| rule.source == *source).count();
        let keep = rules.keep.iter().filter(|rule| rule.source == *source).count();
        let commands: Vec<&str> =
            rules.command_list.iter().filter(|(_, s)| s == source).map(|(c, _)| c.as_str()).collect();
        let mut parts = vec![format!("{columns} column rules")];
        if keep > 0 {
            parts.push(format!("{keep} keep rules"));
        }
        if !commands.is_empty() {
            parts.push(format!("commands: {}", commands.join(", ")));
        }
        lines.push(format!("  {source}: {}", parts.join(", ")));
    }
    lines.join("\n")
}

pub fn is_token(value: &str) -> bool {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    TOKEN.get_or_init(|| Regex::new(r"^[A-Z]+_[0-9a-f]{6}$").unwrap()).is_match(value)
}

pub fn is_nullish(value: &str) -> bool {
    matches!(value, "" | "NULL" | "null" | "nil" | "∅" | "(null)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_columns_get_their_kind() {
        let cases = [
            ("first_name", "PERSON"),
            ("last_name", "PERSON"),
            ("firstname", "PERSON"),
            ("middleNames", "PERSON"),
            ("beneficiary_name", "PERSON"),
            ("creditor_name", "PERSON"),
            ("debitor_name", "PERSON"),
            ("recipient_name", "PERSON"),
            ("billing_details.name", "PERSON"),
            ("customer.name", "PERSON"),
            ("user.name", "PERSON"),
            ("embossedName", "PERSON"),
            ("email", "EMAIL"),
            ("\"email\"", "EMAIL"),
            ("email_address", "EMAIL"),
            ("email_contact", "EMAIL"),
            ("iban", "IBAN"),
            ("phone_number", "PHONE"),
            ("mobile", "PHONE"),
            ("birth_date", "BIRTHDATE"),
            ("birthday", "BIRTHDATE"),
            ("dob", "BIRTHDATE"),
            ("placeOfBirth", "BIRTHPLACE"),
            ("birthCountry", "BIRTHPLACE"),
            ("address", "ADDRESS"),
            ("address1", "ADDRESS"),
            ("street", "ADDRESS"),
            ("city", "ADDRESS"),
            ("postcode", "ADDRESS"),
            ("zip_code", "ADDRESS"),
            ("nationality", "NATIONALITY"),
            ("social_security_number", "SSN"),
            ("taxNumber", "SSN"),
            ("organisation_name", "ORG"),
            ("legalName", "ORG"),
            ("company_name", "ORG"),
            ("corporate_name", "ORG"),
            ("last_sign_in_ip", "IP"),
            ("ip", "IP"),
            ("encrypted_password", "SECRET"),
            ("reset_password_token", "SECRET"),
            ("client_secret_ciphertext", "SECRET"),
        ];
        for (column, kind) in cases {
            assert_eq!(column_kind(column), Some(kind), "{column}");
        }
    }

    #[test]
    fn useful_columns_stay_visible() {
        for column in [
            "id",
            "name",
            "state",
            "status",
            "amount",
            "country",
            "bic",
            "kyc_level",
            "user_id",
            "userStatus",
            "created_at",
            "email_id",
            "supplier_name",
            "benefit_name",
            "description",
            "zip",
            "reset_password_sent_at",
            "unicity_token",
        ] {
            assert_eq!(column_kind(column), None, "{column}");
        }
    }

    #[test]
    fn value_patterns_catch_contact_data() {
        let kinds = |text: &str| {
            value_patterns().iter().filter(|(re, _)| re.is_match(text)).map(|(_, k)| *k).collect::<Vec<_>>()
        };
        assert_eq!(kinds("jean.dupont@example.com"), ["EMAIL"]);
        assert_eq!(kinds("FR76 3000 6000 0112 3456 7890 189"), ["IBAN"]);
        assert_eq!(kinds("FR7630006000011234567890189"), ["IBAN"]);
        assert_eq!(kinds("06 12 34 56 78"), ["PHONE"]);
        assert_eq!(kinds("+33612345678"), ["PHONE"]);
        assert_eq!(kinds("from 192.168.1.20"), ["IP"]);
        assert!(kinds("amount 1234.56, id 42, version 1.2.3, at 12:34:56").is_empty());
    }

    #[test]
    fn masked_commands() {
        assert!(masks_command("psql -c 'select 1'"));
        assert!(masks_command("PGOPTIONS='-c statement_timeout=30s' psql \"$URL\" -c q"));
        assert!(masks_command("rtk psql -c q"));
        assert!(masks_command("heroku run \"rails c --sandbox\" -a my-app"));
        assert!(masks_command("stripe customers retrieve cus_123"));
        assert!(masks_command("/opt/homebrew/opt/libpq/bin/psql -h host -c q"));
        assert!(!masks_command("cat privacy-psql.py"));
        assert!(!masks_command("heroku ps"));
        assert!(!masks_command("git status"));
    }

    #[test]
    fn user_config_adds_overrides_and_keeps() {
        let config: Config = toml::from_str(
            r#"
            keep = ["^city$"]
            commands = ["curl"]
            [[column]]
            pattern = "^(nom|prénom|téléphone)$"
            kind = "PERSON"
            [[value]]
            pattern = '\b\d{14}\b'
            kind = "SIRET"
            "#,
        )
        .unwrap();
        let rules = build(vec![(PathBuf::from("notion.toml"), config)]).unwrap();
        assert_eq!(rules.kind("Prénom"), Some("PERSON"));
        assert_eq!(rules.kind("city"), None);
        assert_eq!(rules.kind("first_name"), Some("PERSON"));
        assert_eq!(rules.values[0].1, "SIRET");
        assert_eq!(lookup(&rules, "prénom").unwrap().source, "notion.toml");
        assert_eq!(lookup(&rules, "iban").unwrap().source, "built-in payments");
        assert_eq!(lookup(&rules, "city").unwrap_err().unwrap().source, "notion.toml");
        assert!(rules.commands.is_match("curl https://api.stripe.com/v1/customers"));
    }

    #[test]
    fn invalid_config_is_rejected() {
        let bad_kind: Config = toml::from_str("[[column]]\npattern = \"x\"\nkind = \"person\"").unwrap();
        assert!(build(vec![(PathBuf::from("x.toml"), bad_kind)]).is_err());
        let bad_regex: Config = toml::from_str("[[value]]\npattern = \"(\"\nkind = \"X\"").unwrap();
        assert!(build(vec![(PathBuf::from("x.toml"), bad_regex)]).is_err());
        assert!(toml::from_str::<Config>("colum = []").is_err());
    }

    #[test]
    fn tokens_and_nulls_are_recognized() {
        assert!(is_token("PERSON_d86896"));
        assert!(!is_token("PERSON_d86896x"));
        assert!(!is_token("Jean"));
        assert!(is_nullish("NULL") && is_nullish("") && is_nullish("nil"));
    }
}
