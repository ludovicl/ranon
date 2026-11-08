//! Runs the real command (psql, heroku…) and masks its output, rtk-style.

use crate::mask::Masker;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// First executable named `name` in PATH that is not ranon itself (the shim is a symlink to ranon).
pub fn find_real(name: &str) -> Option<PathBuf> {
    let me = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file() && candidate.canonicalize().ok() != me)
}

pub fn tuples_only(args: &[OsString]) -> bool {
    args.iter().filter_map(|a| a.to_str()).any(|a| {
        a == "--tuples-only"
            || (a.starts_with('-')
                && !a.starts_with("--")
                && a[1..].chars().all(|c| c.is_ascii_alphabetic())
                && a.contains('t'))
    })
}

pub fn run(name: &str, args: &[OsString]) -> i32 {
    let Some(real) = find_real(name) else {
        eprintln!("ranon: no real `{name}` found in PATH");
        return 127;
    };
    if name == "psql" && tuples_only(args) {
        eprintln!("ranon: output hidden, rerun without -t/--tuples-only so columns can be masked");
        return 2;
    }
    let mut masker = match Masker::open() {
        Ok(masker) => masker,
        Err(error) => {
            eprintln!("ranon: cannot open state ({error}), output hidden");
            return 2;
        }
    };
    let mut child = match Command::new(&real)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!("ranon: cannot run {}: {error}", real.display());
            return 127;
        }
    };
    let mut stderr_pipe = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        stderr_pipe.read_to_end(&mut buffer).ok();
        buffer
    });
    let mut stdout = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut stdout).ok();
    let stderr = stderr_reader.join().unwrap_or_default();
    let status = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);

    let out = masker.mask_text(&String::from_utf8_lossy(&stdout));
    let err = masker.mask_text(&String::from_utf8_lossy(&stderr));
    if let Err(error) = masker.persist() {
        eprintln!("ranon: cannot save token map ({error}), output hidden");
        return 2;
    }
    std::io::stdout().write_all(out.as_bytes()).ok();
    std::io::stderr().write_all(err.as_bytes()).ok();
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn detects_tuples_only_flags() {
        assert!(tuples_only(&args(&["-t", "-c", "select 1"])));
        assert!(tuples_only(&args(&["-At", "-c", "select 1"])));
        assert!(tuples_only(&args(&["--tuples-only"])));
    }

    #[test]
    fn ignores_other_flags_and_sql_text() {
        assert!(!tuples_only(&args(&["-A", "-c", "select -t from x"])));
        assert!(!tuples_only(&args(&["--set", "x=1", "--csv"])));
        assert!(!tuples_only(&args(&["-d", "app_test"])));
    }
}
