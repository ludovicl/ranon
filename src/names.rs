//! Dictionary of known person names, stored as salted hashes only (never the names themselves).

use crate::store::{Store, private_file};
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::sync::OnceLock;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// Words too common to mask even if someone carries them as a name.
const STOPWORDS: &[&str] = &[
    "les", "des", "une", "pour", "avec", "sans", "sur", "sous", "dans", "par", "pas", "est", "son", "mon", "ton",
    "ses", "mes", "aux", "que", "qui", "quoi", "mais", "donc", "car", "tout", "tous", "elle", "lui", "nous", "vous",
    "ils", "the", "and", "for", "not", "null", "nil", "true", "false", "none", "test", "admin", "user", "users",
    "select", "from", "where", "order", "limit", "group", "join", "row", "rows", "record", "error", "warning",
];

pub struct Names {
    hashes: Vec<u64>,
}

pub fn word_regex() -> &'static Regex {
    static WORD: OnceLock<Regex> = OnceLock::new();
    WORD.get_or_init(|| Regex::new(r"[\p{L}\p{N}_]+").unwrap())
}

pub fn normalize(word: &str) -> String {
    word.nfd().filter(|c| !is_combining_mark(*c)).collect::<String>().to_lowercase()
}

fn key(store: &Store, normalized: &str) -> u64 {
    let digest = store.digest(&format!("name:{normalized}"));
    u64::from_le_bytes(digest[..8].try_into().unwrap())
}

fn indexable(normalized: &str) -> bool {
    normalized.chars().count() >= 3 && normalized.chars().all(char::is_alphabetic) && !STOPWORDS.contains(&normalized)
}

impl Names {
    pub fn load(store: &Store) -> Option<Self> {
        let bytes = fs::read(store.path("names.bin")).ok()?;
        let hashes = bytes.as_chunks::<8>().0.iter().map(|chunk| u64::from_le_bytes(*chunk)).collect();
        Some(Self { hashes })
    }

    pub fn contains(&self, store: &Store, word: &str) -> bool {
        let normalized = normalize(word);
        indexable(&normalized) && self.hashes.binary_search(&key(store, &normalized)).is_ok()
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// Splits every line into words and stores their hashes, merged with the existing dictionary.
    pub fn build<'a>(store: &Store, lines: impl Iterator<Item = &'a str>, replace: bool) -> io::Result<usize> {
        let mut hashes: HashSet<u64> = match (replace, Self::load(store)) {
            (false, Some(existing)) => existing.hashes.into_iter().collect(),
            _ => HashSet::new(),
        };
        for line in lines {
            for word in word_regex().find_iter(line) {
                let normalized = normalize(word.as_str());
                if indexable(&normalized) {
                    hashes.insert(key(store, &normalized));
                }
            }
        }
        let mut sorted: Vec<u64> = hashes.into_iter().collect();
        sorted.sort_unstable();
        let bytes: Vec<u8> = sorted.iter().flat_map(|h| h.to_le_bytes()).collect();
        private_file(&store.path("names.bin"), false)?.write_all(&bytes)?;
        Ok(sorted.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_and_strips_accents() {
        assert_eq!(normalize("Hélène"), "helene");
        assert_eq!(normalize("ÇAĞLAR"), "caglar");
    }

    #[test]
    fn short_numeric_and_stop_words_are_not_indexed() {
        assert!(indexable("dupont"));
        assert!(!indexable("le"));
        assert!(!indexable("abc1"));
        assert!(!indexable("pour"));
        assert!(!indexable("null"));
    }
}
