# AGENTS.md

Guide for coding agents (and humans) working on ranon. `CLAUDE.md` imports this file.

## What ranon is

A single Rust binary that masks personal data in command and tool output before it reaches an LLM agent. Sensitive values become stable tokens (`PERSON_d86896`); the token → value map stays on the user's machine. See `README.md` for usage.

## Layout

| Path | Role |
|---|---|
| `src/main.rs` | CLI (clap), busybox-style shim dispatch on `argv[0]`, `dict build`, `init`, `rules`, `forget` |
| `src/mask.rs` | The masking passes, in order: JSON documents, tables, key/value pairs, value patterns, names |
| `src/rules/builtin.rs` | Built-in rules, grouped by source (`payments`, `common`, `collaboration`, `security`, `databases`) |
| `src/rules/mod.rs` | Loads built-in rules and user files (`rules.toml`, `rules.d/*.toml`), tracks where each rule comes from |
| `src/names.rs` | Dictionary of person names, stored as salted hashes only |
| `src/store.rs` | State directory: salt, token map, HMAC tokens (collision-free), unmasking, forgetting |
| `src/proxy.rs` | Runs the real command found further down `PATH` and masks its output |
| `src/hook.rs` | Claude Code hook: `PostToolUse` masks output, `PreToolUse` restores real values before writes, `hook.log` |
| `src/doctor.rs` | `ranon doctor`: checks binary, rules, state, dictionary and Claude Code hooks |
| `tests/cli.rs` | Integration tests running the real binary with an isolated `RANON_HOME` and `RANON_CONFIG_DIR` |

## Invariants (do not break)

- **Fail closed.** When masking cannot run (bad state, invalid rules file, unreadable payload, panic), hide the output; never fall back to the raw text. Keep panics catchable: no `panic = "abort"`. A `PreToolUse` failure may let tokens through, never real data to the model.
- **No real personal data anywhere in the repo**: tests, fixtures, docs, issues and commit messages use synthetic values (`Jean Dupont`, `jean@example.com`, `FR7630006000011234567890189`).
- **Names never printed.** `dict build` pipes names straight into the hasher; `names.bin` holds hashes only.
- **State files are private** (`salt`, `map.tsv`, `names.bin`, mode 600 in a 700 directory) and never committed.
- **Token format is stable**: `KIND_` + 6 lowercase hex characters, `KIND` in uppercase letters. `is_token` and unmasking depend on it, and existing token maps must keep working.
- **One token, one value.** `Store::token` rehashes a value whose token is already taken; never overwrite or reuse an entry of the map.
- **Nothing personal in `hook.log`**: tool name, outcome and counts only, never a command or a value.
- **Masking is idempotent**: a token is never masked again.
- **Organization-specific rules stay out of the code.** A rule belongs in `builtin.rs` only if it is useful to anyone using that kind of source (Stripe, Notion, SEPA…). Company field names go in the user's `rules.d/` files.

## Adding a rule

1. Pick the source set in `src/rules/builtin.rs`, or add a new `RuleSet` and list it in `SETS` (order matters for value patterns: an earlier pattern masks first).
2. Anchor the pattern (`(^|_)phone$`, not `phone`): names are matched in lowercase and in snake_case, and a loose pattern masks identifiers the model needs.
3. Add a case to `sensitive_columns_get_their_kind` and, if the rule could catch useful columns, one to `useful_columns_stay_visible`.
4. For a new format or source, add an integration test in `tests/cli.rs` with a synthetic sample shaped like the real output.

## Before committing

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same three on Linux and macOS. A change to masking logic should come with a test that fails without it.

## Commits

- Commit or push only when the maintainer asks.
- One logical change per commit. Subject in English, imperative, under 72 characters (`Add JSON pass for Stripe objects`), body explaining why when it is not obvious.
- No AI attribution trailers (`Co-Authored-By`) or mentions of AI tools in commits or PRs.
- Never commit `target/`, state files or a user's `rules.d/`.
