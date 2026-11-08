# ranon

[![CI](https://github.com/ludovicl/ranon/actions/workflows/ci.yml/badge.svg)](https://github.com/ludovicl/ranon/actions/workflows/ci.yml)

**Masks personal data in command output before it reaches an LLM agent.**

Coding agents are most useful when they can query real systems: a read replica, a production console, an API. That output often holds names, emails, IBANs or addresses that should not leave your machine. ranon sits between the command and the agent and replaces each sensitive value with a stable pseudonym:

```text
 id |  first_name   |   last_name   |     email     | organisation_name
----+---------------+---------------+---------------+-------------------
 42 | PERSON_d86896 | PERSON_55fc2c | EMAIL_d2f401  | ORG_87d061
```

Same value, same token: the model can still join, count and compare. The token → value map stays on your machine, so you can turn the model's answer back into real values.

Inspired by [rtk](https://github.com/rtk-ai/rtk): one small binary that works as a command proxy, an agent hook or a stdin filter.

## How it works

```mermaid
flowchart LR
    A["LLM agent<br/>(Claude Code, Codex, …)"] -- "runs psql …" --> B{{ranon}}
    B -- "real command" --> C[("psql / heroku run /<br/>any CLI")]
    C -- "raw output" --> B
    B -- "masked output" --> A
    B <-. "salt, token map,<br/>name hashes" .-> S[("~/.local/state/ranon<br/>local only, mode 700")]
    U(["You"]) -- "ranon unmask" --> S
```

ranon plugs in at one of three places:

| Mode | How | Covers |
|---|---|---|
| **Agent hook** | `ranon hook claude` as a Claude Code `PostToolUse` hook | Bash output of `psql` and `heroku run`, plus any MCP tool you match |
| **PATH shim** | `ranon init` creates `psql → ranon`; put that directory first in the agent's PATH | Any agent harness, including output redirected to a file |
| **Filter** | `ranon psql …`, `… \| ranon mask` | Manual use and scripts |

### The masking pipeline

Every output goes through four passes, from the most structured to the least:

```mermaid
flowchart TD
    IN[/raw output/] --> T
    T["1 · Tables<br/>psql aligned, -x, -A, --csv, rtk formats<br/>mask by column name"] --> K
    K["2 · Key/value pairs<br/>JSON, Ruby hashes, Rails inspect<br/>mask by key"] --> P
    P["3 · Patterns anywhere<br/>email, IBAN, French phone numbers"] --> N
    N["4 · Names in free text<br/>consecutive capitalized words found<br/>in the hashed name dictionary"] --> OUT[/masked output/]
    T -. "separator inside a value" .-> H["row hidden:<br/>[ranon: row hidden, ambiguous format]"]
```

1. **Tables by column name.** Sensitive columns: first/last/full names, email, phone, IBAN, birth date and place, address, nationality, social security or tax number, organisation name ([`src/rules.rs`](src/rules.rs)). Other columns (`id`, `status`, `amount`, `country`, …) stay readable. A row whose separator count does not match the header is hidden rather than guessed.
2. **Key/value pairs by key**: `first_name: "Jean"`, `"email": "…"`, `"last_name"=>"…"`.
3. **Value patterns anywhere**: email, IBAN, French phone numbers.
4. **Full names in free text**: two or more consecutive capitalized words found in the name dictionary (`Jean Dupont`, `DUPONT Marie-Anne`), or one after a title (`M. Dupont`, `Mme Petit`). A lone dictionary word is left alone, because real databases contain junk "names" such as *Bonjour* or *Paris*.

`psql -t` / `--tuples-only` output is hidden: without a header there is no column name to mask by.

### Tokens

```mermaid
flowchart LR
    V["Jean"] --> H["HMAC-SHA256<br/>(local secret salt)"] --> T["PERSON_d86896"]
    T --> M[("map.tsv<br/>PERSON_d86896 → Jean")]
```

- Tokens are `KIND_` plus the first 6 hex characters of an HMAC with a 32-byte random salt created on first run. Without the salt, a token cannot be reversed or brute-forced from a list of common names.
- Masking is idempotent: a token is never masked twice.
- The name dictionary stores salted 64-bit hashes only, never the names.

## Install

Requires Rust 1.88+ on Linux or macOS.

```sh
cargo install --git https://github.com/ludovicl/ranon
```

### Name dictionary (optional, for pass 4)

```sh
export DATABASE_URL=postgres://readonly@replica/app
ranon dict build --sql "select first_name from users union select last_name from users"
# or from any source, one or more names per line:
cut -d, -f2,3 people.csv | ranon dict build --stdin
ranon dict stats
```

Names go straight from `psql` into the hasher; they are never printed. `--db-url-env` reads the URL from another variable, `--replace` starts from an empty dictionary.

### Claude Code hook

In `.claude/settings.json`:

```json
{
  "hooks": {
    "PostToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "ranon hook claude" }] },
      { "matcher": "mcp__notion__.*", "hooks": [{ "type": "command", "command": "ranon hook claude" }] }
    ]
  }
}
```

For Bash, only `psql` and `heroku run` outputs are masked. For any other matched tool, every string of the response is. If masking fails, the output is replaced by an error message (fail closed).

A hook whose binary is missing lets output through, so a `PreToolUse` guard that blocks `psql` when `ranon` is not in `PATH` is a good companion.

### PATH shim (any agent)

```sh
ranon init                 # ~/.local/share/ranon/shims/psql -> ranon
ranon init heroku psql     # several commands
```

Put `~/.local/share/ranon/shims` first in the **agent's** PATH only, not your own shell. The agent keeps typing `psql`; ranon finds the real binary further down the PATH, runs it and masks stdout and stderr, keeping the exit code.

### Manual

```sh
ranon psql "$DATABASE_URL" -c "select …"   # proxy
some-command | ranon mask
pbpaste | ranon unmask                     # restore real values, locally
```

## State

`~/.local/state/ranon/` (override with `RANON_HOME`), directory mode 700, files mode 600:

| File | Content |
|---|---|
| `salt` | 32 random bytes, hex |
| `map.tsv` | token → real value |
| `names.bin` | sorted salted hashes of name words |

**Never share or commit these files.** Anyone with `salt` and `map.tsv` can unmask everything.

## Limits

ranon is a filter, not a security boundary.

- It masks results, not queries: a name you type in a `where` clause is seen by the model.
- Values outside the known columns, keys and patterns, names missing from the dictionary and lone first names in free text go through.
- Calling the real binary by absolute path bypasses the shim.
- For real isolation, give the agent a database role that can only read pseudonymized views.

## Development

```sh
cargo test                                   # unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Integration tests ([`tests/cli.rs`](tests/cli.rs)) run the real binary against synthetic samples in an isolated `RANON_HOME`.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
