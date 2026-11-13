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

/// A column or key name rule. `kind` is `None` for a keep rule: the name stays visible.
#[derive(Debug)]
struct Rule {
    re: Regex,
    kind: Option<&'static str>,
    source: &'static str,
}

pub struct ValuePattern {
    pub re: Regex,
    pub kind: &'static str,
    accepts: Option<fn(&str) -> bool>,
}

impl ValuePattern {
    pub fn accepts(&self, text: &str) -> bool {
        self.accepts.is_none_or(|check| check(text))
    }
}

pub struct Rules {
    /// In priority order, first match wins: user keeps, user columns, built-in keeps, built-in columns.
    names: Vec<Rule>,
    values: Vec<ValuePattern>,
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

/// Compiled pattern and kind of the `[[column]]` or `[[value]]` entries of a user file.
fn user_rules(list: Vec<RuleConfig>, source: &'static str) -> Result<Vec<(Regex, &'static str)>, String> {
    list.into_iter()
        .map(|rule| {
            if rule.kind.is_empty() || !rule.kind.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(format!("{source}: invalid kind `{}`, use uppercase letters only (PERSON)", rule.kind));
            }
            let re = compile(&rule.pattern).map_err(|e| format!("{source}: {e}"))?;
            Ok((re, leak(rule.kind)))
        })
        .collect()
}

fn build(configs: Vec<(PathBuf, Config)>) -> Result<Rules, String> {
    let mut rules = Rules {
        names: Vec::new(),
        values: Vec::new(),
        commands: Regex::new("$^").unwrap(),
        command_list: Vec::new(),
        files: Vec::new(),
        sources: Vec::new(),
    };
    // user rules come first so they can override a built-in kind or keep
    let mut user_columns = Vec::new();
    for (path, config) in configs {
        let source = leak(path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()));
        for pattern in &config.keep {
            let re = compile(pattern).map_err(|e| format!("{source}: {e}"))?;
            rules.names.push(Rule { re, kind: None, source });
        }
        for (re, kind) in user_rules(config.column, source)? {
            user_columns.push(Rule { re, kind: Some(kind), source });
        }
        for (re, kind) in user_rules(config.value, source)? {
            rules.values.push(ValuePattern { re, kind, accepts: None });
        }
        rules.command_list.extend(config.commands.into_iter().map(|command| (command, source)));
        rules.files.push(path);
        rules.sources.push(source);
    }
    rules.names.extend(user_columns);
    let mut builtin_columns = Vec::new();
    for set in builtin::SETS {
        let source = leak(format!("built-in {}", set.name));
        rules.sources.push(source);
        for pattern in set.keep {
            rules.names.push(Rule { re: Regex::new(pattern).unwrap(), kind: None, source });
        }
        for (pattern, kind) in set.columns {
            builtin_columns.push(Rule { re: Regex::new(pattern).unwrap(), kind: Some(kind), source });
        }
        for value in set.values {
            let re = Regex::new(value.pattern).unwrap();
            rules.values.push(ValuePattern { re, kind: value.kind, accepts: value.accepts });
        }
        rules.command_list.extend(set.commands.iter().map(|command| (command.to_string(), source)));
    }
    rules.names.extend(builtin_columns);
    let alternatives: Vec<String> = rules
        .command_list
        .iter()
        .map(|(command, _)| command.split_whitespace().map(regex::escape).collect::<Vec<_>>().join(r"\s+"))
        .collect();
    rules.commands = compile(&format!(r"(^|[\s;&|(/])({})(\s|$)", alternatives.join("|")))?;
    Ok(rules)
}

/// `dateOfBirth`, `IPAddress` and `Last Name` as `date_of_birth`, `ip_address` and `last_name`.
fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut snake = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, ' ' | '-') {
            snake.push('_');
            continue;
        }
        let after_lower = i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
        let acronym_end =
            i > 0 && chars[i - 1].is_uppercase() && chars.get(i + 1).is_some_and(|next| next.is_lowercase());
        if c.is_uppercase() && (after_lower || acronym_end) {
            snake.push('_');
        }
        snake.extend(c.to_lowercase());
    }
    snake
}

/// The first rule matching a name, as written in lowercase or in snake_case.
fn lookup<'a>(rules: &'a Rules, name: &str) -> Option<&'a Rule> {
    let name = name.trim().trim_matches(['"', ':']);
    let forms = [name.to_lowercase(), snake_case(name)];
    rules.names.iter().find(|rule| forms.iter().any(|form| rule.re.is_match(form)))
}

impl Rules {
    fn kind(&self, name: &str) -> Option<&'static str> {
        lookup(self, name).and_then(|rule| rule.kind)
    }
}

/// Kind of a column or key name (`first_name`, `"email"`, `billing_details.name`), if sensitive.
pub fn column_kind(name: &str) -> Option<&'static str> {
    rules().kind(name)
}

/// Which rule applies to a name, and where it comes from.
pub fn explain(name: &str) -> String {
    match lookup(rules(), name) {
        Some(Rule { kind: Some(kind), source, .. }) => format!("{kind} ({source})"),
        Some(keep) => format!("kept visible (keep in {})", keep.source),
        None => "kept visible".to_string(),
    }
}

pub fn value_patterns() -> &'static [ValuePattern] {
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
        let from_source = || rules.names.iter().filter(|rule| rule.source == *source);
        let columns = from_source().filter(|rule| rule.kind.is_some()).count();
        let keep = from_source().filter(|rule| rule.kind.is_none()).count();
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
            ("dateOfBirth", "BIRTHDATE"),
            ("billingAddress", "ADDRESS"),
            ("clientIP", "IP"),
            ("userEmail", "EMAIL"),
            ("socialSecurityNumber", "SSN"),
            ("Last Name", "PERSON"),
            ("real_name", "PERSON"),
            ("realName", "PERSON"),
            ("display_name_normalized", "PERSON"),
            ("userName", "PERSON"),
            ("author_first_name", "PERSON"),
            ("contact_name", "PERSON"),
            ("phone_number_contact", "PHONE"),
            ("mobile_phone", "PHONE"),
            ("issuer_iban", "IBAN"),
            ("iban_ciphertext", "IBAN"),
            ("recipient_address", "ADDRESS"),
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
            "mobile_app_version",
            "phone_code",
            "phone_verified_at",
            "iban_verified_at",
            "address_id",
            "emailStatus",
            "access_token_expires_at",
        ] {
            assert_eq!(column_kind(column), None, "{column}");
        }
    }

    #[test]
    fn value_patterns_catch_contact_data() {
        let kinds = |text: &str| {
            value_patterns()
                .iter()
                .filter(|pattern| pattern.re.find_iter(text).any(|found| pattern.accepts(found.as_str())))
                .map(|pattern| pattern.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(kinds("jean.dupont@example.com"), ["EMAIL"]);
        assert_eq!(kinds("FR76 3000 6000 0112 3456 7890 189"), ["IBAN"]);
        assert_eq!(kinds("FR7630006000011234567890189"), ["IBAN"]);
        assert_eq!(kinds("06 12 34 56 78"), ["PHONE"]);
        assert_eq!(kinds("+33612345678"), ["PHONE"]);
        assert_eq!(kinds("+33 6 12 34 56 78"), ["PHONE"]);
        assert!(kinds("uuid 3f2a-0123-4567-89ab").is_empty());
        assert!(kinds("order AB12CDEF3456, code FR76ABCD1234, sku XY99 ABCD EFGH IJKL").is_empty());
        assert_eq!(kinds("from 192.168.1.20"), ["IP"]);
        assert!(kinds("amount 1234.56, id 42, version 1.2.3, at 12:34:56").is_empty());
    }

    #[test]
    fn masked_commands() {
        assert!(masks_command("psql -c 'select 1'"));
        assert!(masks_command("PGOPTIONS='-c statement_timeout=30s' psql \"$URL\" -c q"));
        assert!(masks_command("rtk psql -c q"));
        assert!(masks_command("heroku run \"rails c --sandbox\" -a my-app"));
        assert!(masks_command("heroku logs -n 300 -a my-app"));
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
        assert_eq!(rules.values[0].kind, "SIRET");
        assert_eq!(lookup(&rules, "prénom").unwrap().source, "notion.toml");
        assert_eq!(lookup(&rules, "iban").unwrap().source, "built-in payments");
        assert_eq!(lookup(&rules, "city").unwrap().source, "notion.toml");
        assert!(rules.commands.is_match("curl https://api.stripe.com/v1/customers"));
    }

    #[test]
    fn user_column_overrides_a_builtin_keep() {
        let config: Config = toml::from_str("[[column]]\npattern = '^national_id$'\nkind = \"SSN\"").unwrap();
        let rules = build(vec![(PathBuf::from("x.toml"), config)]).unwrap();
        assert_eq!(rules.kind("national_id"), Some("SSN"));
        assert_eq!(rules.kind("nationalId"), Some("SSN"));
        assert_eq!(rules.kind("address_id"), None);
    }

    #[test]
    fn snake_case_splits_words_and_acronyms() {
        for (name, snake) in [
            ("dateOfBirth", "date_of_birth"),
            ("IPAddress", "ip_address"),
            ("userID", "user_id"),
            ("Last Name", "last_name"),
            ("address1", "address1"),
            ("IBAN", "iban"),
            ("billing_details.name", "billing_details.name"),
        ] {
            assert_eq!(snake_case(name), snake);
        }
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
