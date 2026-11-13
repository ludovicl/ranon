//! Masking passes: JSON documents by key path, tables by column name, key/value pairs, value patterns,
//! then known names in free text.

use crate::names::{Names, word_regex};
use crate::rules::{column_kind, is_nullish, is_token, value_patterns};
use crate::store::Store;
use regex::{Captures, Regex};
use serde_json::Value;
use std::io;
use std::sync::OnceLock;

pub struct Masker {
    pub store: Store,
    names: Option<Names>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

impl Masker {
    pub fn open() -> io::Result<Self> {
        crate::rules::load().map_err(io::Error::other)?;
        let store = Store::open()?;
        let names = Names::load(&store);
        Ok(Self { store, names })
    }

    pub fn persist(&mut self) -> io::Result<()> {
        self.store.persist()
    }

    pub fn mask_text(&mut self, text: &str) -> String {
        let text = self.mask_json_documents(text);
        let text = self.mask_tables(&text);
        let text = self.mask_key_values(&text);
        let text = self.mask_patterns(&text);
        self.mask_names(&text)
    }

    /// JSON documents (Stripe CLI, API responses, MCP tools), masked by key with the parent as context.
    /// One is looked for at the start of every line (the whole output, JSON lines, JSON after a banner)
    /// and where a line ends by opening one (the `inspect` of an API object).
    fn mask_json_documents(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while !rest.is_empty() {
            let line_end = rest.find('\n').map_or(rest.len(), |newline| newline + 1);
            let indent = rest.len() - rest.trim_start_matches([' ', '\t']).len();
            // also a document opened at the end of a line: `#<Treezor::User id=1> JSON: {`
            let opened = rest[..line_end].trim_end().len().saturating_sub(1);
            let found = [indent, opened].into_iter().find_map(|at| Some((at, leading_json(&rest[at..])?)));
            let Some((indent, (mut value, len))) = found else {
                out.push_str(&rest[..line_end]);
                rest = &rest[line_end..];
                continue;
            };
            let source = &rest[indent..indent + len];
            self.mask_json(&mut value);
            let masked = if source.contains('\n') {
                serde_json::to_string_pretty(&value)
            } else {
                serde_json::to_string(&value)
            };
            out.push_str(&rest[..indent]);
            out.push_str(&masked.unwrap_or_else(|_| "[ranon: JSON hidden]".to_string()));
            rest = &rest[indent + len..];
        }
        out
    }

    pub fn mask_json(&mut self, value: &mut Value) {
        self.mask_json_at(value, "", None);
    }

    /// The kind of a key comes from `parent.key` (parent = the object's `"object"` type, as in Stripe and Notion,
    /// else the enclosing key), then the key alone, then the enclosing sensitive key (`address`, `dob`…).
    fn mask_json_at(&mut self, value: &mut Value, parent: &str, inherited: Option<&'static str>) {
        match value {
            Value::Object(map) => {
                let context = map.get("object").and_then(Value::as_str).unwrap_or(parent).to_string();
                let holder = holder_kind(map.keys());
                for (key, child) in map.iter_mut() {
                    let kind = column_kind(&format!("{context}.{key}"))
                        .or_else(|| column_kind(key))
                        .or_else(|| holder.filter(|_| is_plain_name(key)))
                        .or(inherited);
                    self.mask_json_at(child, key, kind);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.mask_json_at(item, parent, inherited)),
            Value::String(text) => {
                if let Some(kind) = inherited {
                    *text = self.mask_cell(kind, text);
                }
            }
            Value::Number(number) => {
                if let Some(kind) = inherited {
                    *value = Value::String(self.store.token(kind, &number.to_string()));
                }
            }
            _ => {}
        }
    }

    fn mask_cell(&mut self, kind: &str, cell: &str) -> String {
        let value = cell.trim();
        if is_nullish(value) || is_token(value) {
            return cell.to_string();
        }
        cell.replacen(value, &self.store.token(kind, value), 1)
    }

    fn mask_tables(&mut self, text: &str) -> String {
        let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
        let mut i = 0;
        while i < lines.len() {
            if let Some(masked) = self.mask_psql_record_line(&lines, i) {
                lines[i] = masked;
                i += 1;
            } else if let Some(masked) = self.mask_rtk_record_line(&lines[i]) {
                lines[i] = masked;
                i += 1;
            } else {
                i = self.mask_table_at(&mut lines, i);
            }
        }
        lines.join("\n")
    }

    /// psql -x: a "col | value" line right after "-[ RECORD n ]-" or another record line.
    fn mask_psql_record_line(&mut self, lines: &[String], i: usize) -> Option<String> {
        static LINE: OnceLock<Regex> = OnceLock::new();
        static PREV: OnceLock<Regex> = OnceLock::new();
        if i == 0 || lines[i].contains('\t') {
            return None;
        }
        let prev = &lines[i - 1];
        if !(prev.starts_with("-[ RECORD") || re(&PREV, r"^\s*\w+\s*\|").is_match(prev)) {
            return None;
        }
        let c = re(&LINE, r"^(\s*)(\w+)(\s*\|\s?)(.*)$").captures(&lines[i])?;
        let value = &c[4];
        Some(match column_kind(&c[2]).filter(|_| !value.contains('|')) {
            Some(kind) => format!("{}{}{}{}", &c[1], &c[2], &c[3], self.mask_cell(kind, value)),
            None => lines[i].clone(),
        })
    }

    /// rtk -x: "[1] col=val col=val".
    fn mask_rtk_record_line(&mut self, line: &str) -> Option<String> {
        static FIELD: OnceLock<Regex> = OnceLock::new();
        if !is_rtk_record(line) {
            return None;
        }
        let (prefix, rest) = line.split_at(line.find("] ")? + 2);
        let starts: Vec<usize> =
            std::iter::once(0).chain(re(&FIELD, r" \w+=").find_iter(rest).map(|m| m.start() + 1)).collect();
        let fields: Vec<String> = starts
            .iter()
            .enumerate()
            .map(|(n, &start)| {
                let end = starts.get(n + 1).map_or(rest.len(), |next| next - 1);
                let field = &rest[start..end];
                match field.split_once('=').and_then(|(key, value)| column_kind(key).map(|kind| (key, value, kind))) {
                    Some((key, value, kind)) => format!("{key}={}", self.mask_cell(kind, value)),
                    None => field.to_string(),
                }
            })
            .collect();
        Some(format!("{prefix}{}", fields.join(" ")))
    }

    /// Table header at `i`: tab (rtk), pipe (psql aligned / -A) or comma (--csv). Returns the next line to read.
    fn mask_table_at(&mut self, lines: &mut Vec<String>, i: usize) -> usize {
        static SEPARATOR: OnceLock<Regex> = OnceLock::new();
        static HEADER_CELL: OnceLock<Regex> = OnceLock::new();
        let single_column = lines.get(i + 1).is_some_and(|next| re(&SEPARATOR, r"^-+$").is_match(next));
        for sep in ['\t', '|', ','] {
            if !(lines[i].contains(sep) || single_column) {
                continue;
            }
            let header = split_cells(&lines[i], sep);
            if !header.iter().all(|h| re(&HEADER_CELL, r"^\s*[\w ?]+\s*$").is_match(h)) {
                continue;
            }
            let end = self.mask_delimited(lines, i, sep, &header);
            if end > i + 1 {
                return end;
            }
        }
        i + 1
    }

    /// Masks the rows under a header line. Returns the index after the table.
    fn mask_delimited(&mut self, lines: &mut Vec<String>, i: usize, sep: char, header: &[String]) -> usize {
        static SEPARATOR: OnceLock<Regex> = OnceLock::new();
        static FOOTER: OnceLock<Regex> = OnceLock::new();
        let holder = holder_kind(header.iter());
        let kinds: Vec<Option<&str>> =
            header.iter().map(|h| column_kind(h).or_else(|| holder.filter(|_| is_plain_name(h)))).collect();
        if kinds.iter().all(Option::is_none) {
            return i + 1;
        }
        let mut j = i + 1;
        if lines.get(j).is_some_and(|l| re(&SEPARATOR, r"^\s*-+(\+-+)*\s*$").is_match(l)) {
            j += 1;
        }
        while j < lines.len() {
            if sep == ',' {
                join_quoted_record(lines, j);
            }
            let line = lines[j].trim();
            // an rtk record starts the next result when queries are chained without a blank line
            if line.is_empty() || re(&FOOTER, r"^\(\d+ rows?\)$").is_match(line) || is_rtk_record(line) {
                break;
            }
            let cells = split_cells(&lines[j], sep);
            lines[j] = if cells.len() == header.len() {
                let masked: Vec<String> = kinds
                    .iter()
                    .zip(&cells)
                    .map(|(kind, cell)| kind.map_or_else(|| cell.clone(), |k| self.mask_cell(k, cell)))
                    .collect();
                join_cells(&masked, sep)
            } else {
                // a separator inside a value makes the row ambiguous: hide it rather than leak it
                "[ranon: row hidden, ambiguous format]".to_string()
            };
            j += 1;
        }
        j
    }

    /// `first_name: "Jean"`, `"email": "a@b.fr"`, `"last_name"=>"Dupont"`, `iban="FR76…"`,
    /// JSON inside a CSV cell (`""email"": ""a@b.fr""`) and YAML-like lines (`last_name: Dupont`).
    fn mask_key_values(&mut self, text: &str) -> String {
        static QUOTED: OnceLock<Regex> = OnceLock::new();
        static CSV_QUOTED: OnceLock<Regex> = OnceLock::new();
        static UNQUOTED_LINE: OnceLock<Regex> = OnceLock::new();
        const KEY: &str = "(?P<key>[A-Za-z_][A-Za-z0-9_]*)";
        let patterns = [
            re(&QUOTED, &format!(r#"{KEY}(?P<sep>"?\s*(?::|=>|=)\s*")(?P<value>(?:[^"\\\n]|\\.)*)(?P<end>")"#)),
            re(&CSV_QUOTED, &format!(r#"{KEY}(?P<sep>""\s*:\s*"")(?P<value>[^"\n]*)(?P<end>"")"#)),
            re(
                &UNQUOTED_LINE,
                &format!(
                    r#"(?m)^(?P<lead>[ \t]*(?:- )?){KEY}(?P<sep>:[ \t]+)(?P<value>[^\s"'\[{{|>&*!#].*)(?P<end>)$"#
                ),
            ),
        ];
        let mut text = text.to_string();
        for pattern in patterns {
            text = pattern
                .replace_all(&text, |c: &Captures| match column_kind(&c["key"]) {
                    Some(kind) => {
                        let lead = c.name("lead").map_or("", |lead| lead.as_str());
                        format!("{lead}{}{}{}{}", &c["key"], &c["sep"], self.mask_cell(kind, &c["value"]), &c["end"])
                    }
                    None => c[0].to_string(),
                })
                .into_owned();
        }
        text
    }

    fn mask_patterns(&mut self, text: &str) -> String {
        let mut text = text.to_string();
        for pattern in value_patterns() {
            text = pattern
                .re
                .replace_all(&text, |c: &Captures| match pattern.accepts(&c[0]) {
                    true => self.store.token(pattern.kind, &c[0]),
                    false => c[0].to_string(),
                })
                .into_owned();
        }
        text
    }

    /// Full names in free text: two or more consecutive capitalized words of the name dictionary
    /// ("Jean Dupont", "DUPONT Marie-Anne"), or one after a title ("M. Dupont").
    /// A lone dictionary word is left alone: the database holds junk "names" like Bonjour or Paris.
    fn mask_names(&mut self, text: &str) -> String {
        const TITLES: &[&str] = &["M", "Mr", "Mme", "Mlle", "Monsieur", "Madame", "Mademoiselle"];
        let Some(names) = &self.names else { return text.to_string() };
        let words: Vec<(usize, usize, bool)> = word_regex()
            .find_iter(text)
            .map(|m| {
                let word = m.as_str();
                let candidate = word.chars().next().is_some_and(char::is_uppercase)
                    && !word.contains('_')
                    && !word.chars().any(|ch| ch.is_ascii_digit())
                    && !TITLES.contains(&word)
                    && names.contains(&self.store, word);
                (m.start(), m.end(), candidate)
            })
            .collect();
        let joined = |a: usize, b: usize| is_name_gap(&text[words[a].1..words[b].0]);
        let mut mask = vec![false; words.len()];
        for i in 0..words.len() {
            if !words[i].2 {
                continue;
            }
            let after_title = i > 0
                && TITLES.contains(&&text[words[i - 1].0..words[i - 1].1])
                && matches!(&text[words[i - 1].1..words[i].0], " " | ". ");
            let in_pair = (i > 0 && words[i - 1].2 && joined(i - 1, i))
                || (i + 1 < words.len() && words[i + 1].2 && joined(i, i + 1));
            mask[i] = after_title || in_pair;
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (i, &(start, end, _)) in words.iter().enumerate() {
            if mask[i] {
                out.push_str(&text[last..start]);
                out.push_str(&self.store.token("PERSON", &text[start..end]));
                last = end;
            }
        }
        out.push_str(&text[last..]);
        out
    }
}

/// What may sit between the first and last name of one person: a space or a hyphen in prose, or the
/// separator of a record printed without column names (`a | b`, tabs, a Ruby or JSON array `"a", "b"`).
/// A plain comma is left out: in prose it separates words of different people ("Bonjour, Jean").
fn is_name_gap(gap: &str) -> bool {
    if matches!(gap, " " | "-") {
        return true;
    }
    !gap.contains('\n') && matches!(gap.trim_matches([' ', '\t']), "" | "|" | "\", \"" | "\",\"" | "', '" | "','")
}

fn is_plain_name(key: &str) -> bool {
    key.trim().eq_ignore_ascii_case("name")
}

/// A bare `name` beside an IBAN is the account holder (a Treezor beneficiary, a row of a bank accounts table):
/// the kind it gets when the object or the table has an IBAN, `None` otherwise.
fn holder_kind<Key: AsRef<str>>(mut keys: impl Iterator<Item = Key>) -> Option<&'static str> {
    keys.any(|key| column_kind(key.as_ref().trim()) == Some("IBAN")).then_some("PERSON")
}

/// rtk -x: "[1] col=val col=val".
fn is_rtk_record(line: &str) -> bool {
    static LINE: OnceLock<Regex> = OnceLock::new();
    re(&LINE, r"^\[\d+\] \w+=").is_match(line)
}

/// A CSV record whose quoted value holds line breaks (a Slack message) spans several lines: makes it one.
fn join_quoted_record(lines: &mut Vec<String>, at: usize) {
    while lines[at].matches('"').count() % 2 == 1 && at + 1 < lines.len() {
        let next = lines.remove(at + 1);
        lines[at].push('\n');
        lines[at].push_str(&next);
    }
}

/// The JSON object or array `text` starts with, and its length in bytes.
fn leading_json(text: &str) -> Option<(Value, usize)> {
    if !text.starts_with(['{', '[']) {
        return None;
    }
    let mut documents = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let value = documents.next()?.ok()?;
    Some((value, documents.byte_offset()))
}

fn split_cells(line: &str, sep: char) -> Vec<String> {
    if sep == ',' {
        let mut reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_reader(line.as_bytes());
        if let Some(Ok(record)) = reader.records().next() {
            return record.iter().map(String::from).collect();
        }
    }
    line.split(sep).map(String::from).collect()
}

fn join_cells(cells: &[String], sep: char) -> String {
    if sep != ',' {
        return cells.join(&sep.to_string());
    }
    let mut writer = csv::WriterBuilder::new().has_headers(false).from_writer(Vec::new());
    writer.write_record(cells).ok();
    String::from_utf8(writer.into_inner().unwrap_or_default()).unwrap_or_default().trim_end().to_string()
}
