//! ranon: masks personal data in command output before it reaches an LLM.
//!
//! Like rtk, one binary plays several roles:
//! - `ranon psql …` (or a `psql` symlink to ranon placed first in the agent's PATH) runs the real
//!   command and masks its output, whatever the agent harness;
//! - `ranon hook claude` is a Claude Code PostToolUse hook;
//! - `ranon mask` / `ranon unmask` filter stdin;
//! - `ranon dict build` hashes the person names of your own database to catch them in free text.

#[cfg(not(unix))]
compile_error!("ranon supports Unix-like systems only (Linux, macOS)");

mod hook;
mod mask;
mod names;
mod proxy;
mod rules;
mod store;

use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(version, about = "Masks personal data in command output before it reaches an LLM")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mask stdin to stdout
    Mask,
    /// Restore real values in stdin from the local token map
    Unmask,
    /// Agent hook (supported: claude)
    Hook { agent: String },
    /// Manage the hashed dictionary of person names
    Dict {
        #[command(subcommand)]
        action: DictAction,
    },
    /// Create shims (symlinks to ranon) for the given commands, to put first in the agent's PATH
    Init {
        #[arg(long, default_value = "~/.local/share/ranon/shims")]
        shim_dir: String,
        #[arg(default_values_t = vec!["psql".to_string()])]
        commands: Vec<String>,
    },
    /// Run a command and mask its output: `ranon psql -c "select …"`
    #[command(external_subcommand)]
    Run(Vec<OsString>),
}

#[derive(Subcommand)]
enum DictAction {
    /// Hash names returned by a SQL query (`--sql`) or read from stdin (`--stdin`), one or more per line
    Build {
        #[arg(long, conflicts_with = "sql")]
        stdin: bool,
        /// Environment variable holding the database URL
        #[arg(long, default_value = "DATABASE_URL")]
        db_url_env: String,
        /// Query returning name columns, e.g. "select first_name from users union select last_name from users"
        #[arg(long, required_unless_present = "stdin")]
        sql: Option<String>,
        /// Start from an empty dictionary instead of merging
        #[arg(long)]
        replace: bool,
    },
    /// Show dictionary size
    Stats,
}

fn main() {
    let mut args = std::env::args_os();
    let argv0 = args.next().map(PathBuf::from).unwrap_or_default();
    let invoked_as = argv0.file_name().and_then(|n| n.to_str()).unwrap_or("ranon").to_string();
    if invoked_as != "ranon" {
        // called through a shim symlink, e.g. `psql`
        let rest: Vec<OsString> = args.collect();
        std::process::exit(proxy::run(&invoked_as, &rest));
    }
    let code = match Cli::parse().command {
        Cmd::Mask => filter(false),
        Cmd::Unmask => filter(true),
        Cmd::Hook { agent } if agent == "claude" => hook::claude(),
        Cmd::Hook { agent } => fail(&format!("unsupported agent `{agent}`")),
        Cmd::Dict { action: DictAction::Stats } => dict_stats(),
        Cmd::Dict { action: DictAction::Build { stdin, db_url_env, sql, replace } } => {
            dict_build(stdin, &db_url_env, sql.as_deref().unwrap_or_default(), replace)
        }
        Cmd::Init { shim_dir, commands } => init(&shim_dir, &commands),
        Cmd::Run(argv) => match argv.split_first() {
            Some((name, rest)) => proxy::run(&name.to_string_lossy(), rest),
            None => fail("missing command"),
        },
    };
    std::process::exit(code);
}

fn fail(message: &str) -> i32 {
    eprintln!("ranon: {message}");
    1
}

fn filter(unmask: bool) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return fail("stdin is not UTF-8");
    }
    let mut masker = match mask::Masker::open() {
        Ok(masker) => masker,
        Err(error) => return fail(&error.to_string()),
    };
    let output = if unmask {
        let mapping = masker.store.mapping().unwrap_or_default();
        regex::Regex::new(r"\b[A-Z]+_[0-9a-f]{6}\b")
            .unwrap()
            .replace_all(&input, |c: &regex::Captures| mapping.get(&c[0]).cloned().unwrap_or_else(|| c[0].to_string()))
            .into_owned()
    } else {
        let masked = masker.mask_text(&input);
        if let Err(error) = masker.persist() {
            return fail(&error.to_string());
        }
        masked
    };
    std::io::stdout().write_all(output.as_bytes()).ok();
    0
}

fn dict_stats() -> i32 {
    let store = match store::Store::open() {
        Ok(store) => store,
        Err(error) => return fail(&error.to_string()),
    };
    match names::Names::load(&store) {
        Some(names) => println!("{} hashed name words in {}", names.len(), store.path("names.bin").display()),
        None => println!("no dictionary yet: run `ranon dict build`"),
    }
    0
}

fn dict_build(from_stdin: bool, db_url_env: &str, sql: &str, replace: bool) -> i32 {
    let store = match store::Store::open() {
        Ok(store) => store,
        Err(error) => return fail(&error.to_string()),
    };
    let text = if from_stdin {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).ok();
        input
    } else {
        let Ok(url) = std::env::var(db_url_env) else { return fail(&format!("{db_url_env} is not set")) };
        let Some(psql) = proxy::find_real("psql") else { return fail("psql not found") };
        // the names go straight into the hasher, never to stdout
        let output = Command::new(psql)
            .env("PGOPTIONS", "-c statement_timeout=120s")
            .args([url.as_str(), "--no-psqlrc", "-At", "-c", sql])
            .output();
        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(out) => return fail(&format!("psql failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
            Err(error) => return fail(&error.to_string()),
        }
    };
    match names::Names::build(&store, text.lines(), replace) {
        Ok(count) => {
            println!("{count} hashed name words in {}", store.path("names.bin").display());
            0
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn init(shim_dir: &str, commands: &[String]) -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(shim_dir.replacen('~', &home, 1));
    let Ok(exe) = std::env::current_exe() else { return fail("cannot locate ranon binary") };
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return fail(&error.to_string());
    }
    for command in commands {
        let link = dir.join(command);
        std::fs::remove_file(&link).ok();
        if let Err(error) = std::os::unix::fs::symlink(&exe, &link) {
            return fail(&format!("{}: {error}", link.display()));
        }
        println!("shim: {} -> {}", link.display(), exe.display());
    }
    println!("\nPut {} first in the agent's PATH only (not your own shell).", dir.display());
    0
}
