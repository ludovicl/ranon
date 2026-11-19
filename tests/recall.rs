//! Recall benchmark: how much of the personal data in realistic outputs comes out masked, per source and kind.
//!
//! Each file of `tests/recall/` is an output in its real format, with synthetic values marked in place:
//! `[[PERSON:Dupont]]` must be masked, `[[KEEP:active]]` must stay readable. Names are marked word by word,
//! since ranon masks a name in free text word by word. `users.txt` plays the users table of `dict build`.
//!
//! The scores are compared with `tests/recall/baseline.tsv`: a lower score fails, so a change cannot leak
//! more than before. `RANON_BLESS=1 cargo test --test recall` rewrites the baseline after an improvement or a
//! change of the corpus; `-- --nocapture` prints the table.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ranon");

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recall")
}

fn ranon(home: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(BIN)
        .args(args)
        .env("RANON_HOME", home)
        .env("RANON_CONFIG_DIR", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

/// The text without its markers, and the marked values with their kind and number of occurrences.
fn parse(annotated: &str) -> (String, BTreeMap<String, (String, usize)>) {
    let mut text = String::new();
    let mut marked: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut rest = annotated;
    while let Some(start) = rest.find("[[") {
        text.push_str(&rest[..start]);
        let end = rest[start..].find("]]").expect("unclosed marker") + start;
        let (kind, value) = rest[start + 2..end].split_once(':').expect("marker without kind");
        text.push_str(value);
        let entry = marked.entry(value.to_string()).or_insert_with(|| (kind.to_string(), 0));
        assert_eq!(entry.0, kind, "`{value}` is marked with two kinds");
        entry.1 += 1;
        rest = &rest[end + 2..];
    }
    text.push_str(rest);
    (text, marked)
}

/// Occurrences of `value` not glued to a letter or digit (`Jean` is not counted in `Jeanne`).
fn occurrences(text: &str, value: &str) -> usize {
    text.match_indices(value)
        .filter(|(at, _)| {
            let before = text[..*at].chars().next_back();
            let after = text[at + value.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
        .count()
}

/// (source, kind) → (values handled as expected, values marked).
fn score(home: &Path, source: &str, annotated: &str) -> BTreeMap<(String, String), (usize, usize)> {
    let (text, marked) = parse(annotated);
    let masked = ranon(home, &["mask"], &text);
    let mut scores = BTreeMap::new();
    for (value, (kind, count)) in &marked {
        assert_eq!(occurrences(&text, value), *count, "{source}: mark every occurrence of `{value}`");
        let left = occurrences(&masked, value).min(*count);
        let expected = if kind == "KEEP" { left } else { count - left };
        let entry = scores.entry((source.to_string(), kind.clone())).or_insert((0, 0));
        entry.0 += expected;
        entry.1 += count;
    }
    scores
}

fn read_baseline(path: &Path) -> BTreeMap<(String, String), (usize, usize)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            ((fields[0].to_string(), fields[1].to_string()), (fields[2].parse().unwrap(), fields[3].parse().unwrap()))
        })
        .collect()
}

#[test]
fn recall_does_not_regress() {
    let dir = corpus_dir();
    let mut scores = BTreeMap::new();
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| !matches!(path.file_name().and_then(|n| n.to_str()), Some("users.txt" | "baseline.tsv")))
        .collect();
    sources.sort();
    let users = std::fs::read_to_string(dir.join("users.txt")).unwrap();
    for path in &sources {
        // a fresh state per source: what ranon learns from one output must not help the next one here
        let home = std::env::temp_dir().join(format!("ranon-recall-{}-{}", std::process::id(), scores.len()));
        std::fs::create_dir_all(&home).unwrap();
        ranon(&home, &["dict", "build", "--stdin"], &users);
        let source = path.file_name().unwrap().to_str().unwrap();
        scores.extend(score(&home, source, &std::fs::read_to_string(path).unwrap()));
    }

    let mut table = String::from("# source\tkind\tas expected\tmarked\n");
    for ((source, kind), (ok, total)) in &scores {
        table.push_str(&format!("{source}\t{kind}\t{ok}\t{total}\n"));
    }
    println!("{table}");
    let baseline_path = dir.join("baseline.tsv");
    if std::env::var_os("RANON_BLESS").is_some() {
        std::fs::write(&baseline_path, &table).unwrap();
        return;
    }
    let baseline = read_baseline(&baseline_path);
    let mut problems = Vec::new();
    for (key, (ok, total)) in &scores {
        match baseline.get(key) {
            Some((base_ok, base_total)) if base_total != total => {
                problems.push(format!("{key:?}: {total} values marked, {base_total} in the baseline"))
            }
            Some((base_ok, _)) if ok < base_ok => {
                problems.push(format!("{key:?}: {ok}/{total}, was {base_ok}/{total}"))
            }
            Some(_) => {}
            None => problems.push(format!("{key:?}: not in the baseline")),
        }
    }
    problems.extend(baseline.keys().filter(|key| !scores.contains_key(*key)).map(|key| format!("{key:?}: gone")));
    assert!(
        problems.is_empty(),
        "recall changed, rerun with RANON_BLESS=1 if this is intended:\n{}",
        problems.join("\n")
    );
}
