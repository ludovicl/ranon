//! Masking passes: tables by column name, key/value pairs, value patterns, then known names in free text.

use crate::names::{Names, word_regex};
use crate::rules::{column_kind, is_nullish, is_token, value_patterns};
use crate::store::Store;
use regex::{Captures, Regex};
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
        let store = Store::open()?;
        let names = Names::load(&store);
        Ok(Self { store, names })
    }

    pub fn persist(&self) -> io::Result<()> {
        self.store.persist()
    }

    pub fn mask_text(&mut self, text: &str) -> String {
        let text = self.mask_tables(text);
        let text = self.mask_key_values(&text);
        let text = self.mask_patterns(&text);
        self.mask_names(&text)
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
        static LINE: OnceLock<Regex> = OnceLock::new();
        static FIELD: OnceLock<Regex> = OnceLock::new();
        if !re(&LINE, r"^\[\d+\] \w+=").is_match(line) {
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
    fn mask_table_at(&mut self, lines: &mut [String], i: usize) -> usize {
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
    fn mask_delimited(&mut self, lines: &mut [String], i: usize, sep: char, header: &[String]) -> usize {
        static SEPARATOR: OnceLock<Regex> = OnceLock::new();
        static FOOTER: OnceLock<Regex> = OnceLock::new();
        let kinds: Vec<Option<&str>> = header.iter().map(|h| column_kind(h)).collect();
        if kinds.iter().all(Option::is_none) {
            return i + 1;
        }
        let mut j = i + 1;
        if lines.get(j).is_some_and(|l| re(&SEPARATOR, r"^\s*-+(\+-+)*\s*$").is_match(l)) {
            j += 1;
        }
        while j < lines.len() {
            let line = lines[j].trim();
            if line.is_empty() || re(&FOOTER, r"^\(\d+ rows?\)$").is_match(line) {
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

    /// `first_name: "Jean"`, `"email": "a@b.fr"`, `"last_name"=>"Dupont"`, `iban="FR76…"`.
    fn mask_key_values(&mut self, text: &str) -> String {
        static KEY_VALUE: OnceLock<Regex> = OnceLock::new();
        let pattern =
            re(&KEY_VALUE, r#"(?P<key>[A-Za-z_][A-Za-z0-9_]*)(?P<sep>"?\s*(?::|=>|=)\s*")(?P<value>[^"\n]*)""#);
        pattern
            .replace_all(text, |c: &Captures| match column_kind(&c["key"]) {
                Some(kind) => format!("{}{}{}\"", &c["key"], &c["sep"], self.mask_cell(kind, &c["value"])),
                None => c[0].to_string(),
            })
            .into_owned()
    }

    fn mask_patterns(&mut self, text: &str) -> String {
        let mut text = text.to_string();
        for (pattern, kind) in value_patterns() {
            text = pattern.replace_all(&text, |c: &Captures| self.store.token(kind, &c[0])).into_owned();
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
        let joined = |a: usize, b: usize| matches!(&text[words[a].1..words[b].0], " " | "-");
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
