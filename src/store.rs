//! Local state: secret salt, token -> value map and name dictionary, all under ~/.local/state/ranon.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A hex-encoded 32-byte salt; anything much shorter means a truncated file.
const MIN_SALT_LEN: usize = 32;

struct Entry {
    value: String,
    /// Unix time of the first masking, 0 for entries written before timestamps existed.
    first_seen: u64,
}

pub struct Store {
    dir: PathBuf,
    salt: Vec<u8>,
    mapping: HashMap<String, Entry>,
    new_tokens: Vec<String>,
    issued: usize,
}

impl Store {
    pub fn open() -> io::Result<Self> {
        let dir = state_dir();
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let salt = read_or_create_salt(&dir.join("salt"))?;
        let mapping = read_mapping(&dir.join("map.tsv"))?;
        Ok(Self { dir, salt, mapping, new_tokens: Vec::new(), issued: 0 })
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
    /// Two values falling on the same token would be unmasked as one another, so the second one is rehashed.
    pub fn token(&mut self, kind: &str, value: &str) -> String {
        self.issued += 1;
        let value = value.replace(['\t', '\n'], " ");
        let key = canonical(kind, &value);
        let mut attempt = 0u32;
        loop {
            let input = if attempt == 0 { key.clone() } else { format!("{key}\0{attempt}") };
            let token = format!("{kind}_{}", hex::encode(&self.digest(&input)[..3]));
            match self.mapping.get(&token) {
                Some(entry) if canonical(kind, &entry.value) == key => return token,
                Some(_) => attempt += 1,
                None => {
                    self.mapping.insert(token.clone(), Entry { value, first_seen: now() });
                    self.new_tokens.push(token.clone());
                    return token;
                }
            }
        }
    }

    /// How many values were replaced by a token since `open`.
    pub fn issued(&self) -> usize {
        self.issued
    }

    pub fn len(&self) -> usize {
        self.mapping.len()
    }

    pub fn persist(&mut self) -> io::Result<()> {
        if self.new_tokens.is_empty() {
            return Ok(());
        }
        let mut file = private_file(&self.path("map.tsv"), true)?;
        for token in self.new_tokens.drain(..) {
            let entry = &self.mapping[&token];
            writeln!(file, "{token}\t{}\t{}", entry.value, entry.first_seen)?;
        }
        Ok(())
    }

    /// Replaces known tokens with their real values; unknown tokens are left as they are.
    pub fn unmask(&self, text: &str) -> String {
        static TOKEN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        TOKEN
            .get_or_init(|| regex::Regex::new(r"\b[A-Z]+_[0-9a-f]{6}\b").unwrap())
            .replace_all(text, |c: &regex::Captures| {
                self.mapping.get(&c[0]).map_or_else(|| c[0].to_string(), |entry| entry.value.clone())
            })
            .into_owned()
    }

    /// Drops the real values first seen more than `older_than` ago (all of them when `None`).
    /// Their tokens can no longer be unmasked. Returns how many entries were dropped.
    pub fn forget(&mut self, older_than: Option<Duration>) -> io::Result<usize> {
        let before = self.mapping.len();
        let cutoff = older_than.map_or(u64::MAX, |age| now().saturating_sub(age.as_secs()));
        self.mapping.retain(|_, entry| entry.first_seen >= cutoff);
        self.new_tokens.clear();
        let mut lines: Vec<String> = self
            .mapping
            .iter()
            .map(|(token, entry)| format!("{token}\t{}\t{}\n", entry.value, entry.first_seen))
            .collect();
        lines.sort_unstable();
        let temporary = self.path("map.tsv.tmp");
        private_file(&temporary, false)?.write_all(lines.concat().as_bytes())?;
        fs::rename(temporary, self.path("map.tsv"))?;
        Ok(before - self.mapping.len())
    }
}

/// `$RANON_HOME`, else `~/.local/state/ranon`.
pub fn state_dir() -> PathBuf {
    match std::env::var_os("RANON_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/ranon"),
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// The form of a value that decides its token, so that two spellings of the same thing share one.
fn canonical(kind: &str, value: &str) -> String {
    match kind {
        "EMAIL" => value.to_lowercase(),
        "IBAN" => value.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase(),
        _ => value.to_string(),
    }
}

fn read_or_create_salt(path: &Path) -> io::Result<Vec<u8>> {
    // create_new: when two first runs race, only one writes the salt
    match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(mut file) => {
            let mut salt = [0u8; 32];
            fs::File::open("/dev/urandom")?.read_exact(&mut salt)?;
            file.write_all(hex::encode(salt).as_bytes())?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let salt = fs::read_to_string(path)?.trim().as_bytes().to_vec();
    if salt.len() < MIN_SALT_LEN {
        // an empty key would make every token guessable
        return Err(io::Error::other(format!("{} is empty or truncated", path.display())));
    }
    Ok(salt)
}

/// `token<TAB>value[<TAB>first seen]` lines; the first line of a token wins.
fn read_mapping(path: &Path) -> io::Result<HashMap<String, Entry>> {
    let mut mapping = HashMap::new();
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(mapping),
        Err(error) => return Err(error),
    };
    for line in BufReader::new(file).lines() {
        let line = line?;
        let mut fields = line.splitn(3, '\t');
        if let (Some(token), Some(value)) = (fields.next(), fields.next()) {
            let first_seen = fields.next().and_then(|seen| seen.parse().ok()).unwrap_or(0);
            mapping.entry(token.to_string()).or_insert(Entry { value: value.to_string(), first_seen });
        }
    }
    Ok(mapping)
}

pub fn private_file(path: &Path, append: bool) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).mode(0o600);
    if append {
        options.append(true);
    } else {
        options.write(true).truncate(true);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(mapping: &[(&str, &str, u64)]) -> Store {
        let mapping = mapping
            .iter()
            .map(|(token, value, first_seen)| {
                (token.to_string(), Entry { value: value.to_string(), first_seen: *first_seen })
            })
            .collect();
        let dir =
            std::env::temp_dir().join(format!("ranon-store-{}-{:?}", std::process::id(), std::thread::current().id()));
        fs::create_dir_all(&dir).unwrap();
        Store { dir, salt: b"0123456789abcdef0123456789abcdef".to_vec(), mapping, new_tokens: Vec::new(), issued: 0 }
    }

    #[test]
    fn colliding_values_get_distinct_tokens() {
        let mut first = store(&[]);
        let token = first.token("PERSON", "Jean");
        // the token Jean would get is already taken by someone else
        let mut taken = store(&[(&token, "Marie", 1)]);
        let rehashed = taken.token("PERSON", "Jean");
        assert_ne!(rehashed, token);
        assert!(crate::rules::is_token(&rehashed));
        assert_eq!(taken.token("PERSON", "Jean"), rehashed, "the rehashed token is stable");
        assert_eq!(taken.unmask(&format!("{token} {rehashed}")), "Marie Jean");
    }

    #[test]
    fn spellings_of_the_same_email_or_iban_share_a_token() {
        let mut store = store(&[]);
        assert_eq!(store.token("EMAIL", "Jean@Example.com"), store.token("EMAIL", "jean@example.com"));
        assert_eq!(
            store.token("IBAN", "FR76 3000 6000 0112 3456 7890 189"),
            store.token("IBAN", "FR7630006000011234567890189")
        );
        assert_ne!(store.token("PERSON", "Jean"), store.token("PERSON", "jean"));
        let token = store.token("EMAIL", "jean@example.com");
        assert_eq!(store.unmask(&token), "Jean@Example.com", "first spelling kept");
    }

    #[test]
    fn forget_drops_old_entries_only() {
        let recent = now();
        let mut store = store(&[("PERSON_aaaaaa", "Jean", 0), ("PERSON_bbbbbb", "Marie", recent)]);
        assert_eq!(store.forget(Some(Duration::from_secs(3600))).unwrap(), 1);
        assert_eq!(store.unmask("PERSON_aaaaaa PERSON_bbbbbb"), "PERSON_aaaaaa Marie");
        assert_eq!(read_mapping(&store.path("map.tsv")).unwrap().len(), 1);
        assert_eq!(store.forget(None).unwrap(), 1);
        assert!(read_mapping(&store.path("map.tsv")).unwrap().is_empty());
    }

    #[test]
    fn short_salt_is_rejected() {
        let dir = std::env::temp_dir().join(format!("ranon-salt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("salt"), "").unwrap();
        assert!(read_or_create_salt(&dir.join("salt")).is_err());
        fs::remove_file(dir.join("salt")).unwrap();
        assert_eq!(read_or_create_salt(&dir.join("salt")).unwrap().len(), 64);
    }
}
