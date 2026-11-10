//! Local state: secret salt, token -> value map and name dictionary, all under ~/.local/state/ranon.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

pub struct Store {
    dir: PathBuf,
    salt: Vec<u8>,
    new_entries: BTreeMap<String, String>,
}

impl Store {
    pub fn open() -> io::Result<Self> {
        let dir = match std::env::var_os("RANON_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/ranon"),
        };
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let salt_path = dir.join("salt");
        if !salt_path.exists() {
            let mut salt = [0u8; 32];
            fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
            private_file(&salt_path, false)?.write_all(hex::encode(salt).as_bytes())?;
        }
        let salt = fs::read_to_string(&salt_path)?.trim().as_bytes().to_vec();
        Ok(Self { dir, salt, new_entries: BTreeMap::new() })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn digest(&self, data: &str) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.salt).expect("hmac accepts any key length");
        mac.update(data.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Stable pseudonym for a value: same salt + same value = same token.
    pub fn token(&mut self, kind: &str, value: &str) -> String {
        let token = format!("{kind}_{}", hex::encode(&self.digest(value)[..3]));
        self.new_entries.entry(token.clone()).or_insert_with(|| value.to_string());
        token
    }

    pub fn persist(&self) -> io::Result<()> {
        if self.new_entries.is_empty() {
            return Ok(());
        }
        let known: HashSet<String> = self.mapping()?.into_keys().collect();
        let mut file = private_file(&self.path("map.tsv"), true)?;
        for (token, value) in &self.new_entries {
            if !known.contains(token) {
                writeln!(file, "{token}\t{}", value.replace(['\t', '\n'], " "))?;
            }
        }
        Ok(())
    }

    /// Replaces known tokens with their real values; unknown tokens are left as they are.
    pub fn unmask(&self, text: &str, mapping: &HashMap<String, String>) -> String {
        static TOKEN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        TOKEN
            .get_or_init(|| regex::Regex::new(r"\b[A-Z]+_[0-9a-f]{6}\b").unwrap())
            .replace_all(text, |c: &regex::Captures| mapping.get(&c[0]).cloned().unwrap_or_else(|| c[0].to_string()))
            .into_owned()
    }

    pub fn mapping(&self) -> io::Result<HashMap<String, String>> {
        let path = self.path("map.tsv");
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let mut mapping = HashMap::new();
        for line in BufReader::new(fs::File::open(path)?).lines() {
            let line = line?;
            if let Some((token, value)) = line.split_once('\t') {
                mapping.insert(token.to_string(), value.to_string());
            }
        }
        Ok(mapping)
    }
}

pub fn private_file(path: &PathBuf, append: bool) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).mode(0o600);
    if append {
        options.append(true);
    } else {
        options.write(true).truncate(true);
    }
    options.open(path)
}
