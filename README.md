<p align="center"><img src="site/logo.svg" alt="ranon logo" width="96"></p>

# ranon

[![CI](https://github.com/ludovicl/ranon/actions/workflows/ci.yml/badge.svg)](https://github.com/ludovicl/ranon/actions/workflows/ci.yml)

**Masks personal data in command output before it reaches an LLM agent.**

ranon stands for **R**ust **anon**ymizer.

Coding agents are most useful when they can query real systems: a read replica, a production console, an API. That output often holds names, emails, IBANs or addresses that should not leave your machine. ranon sits between the command and the agent and replaces each sensitive value with a stable pseudonym:

![psql output before and after ranon](docs/demo.gif)

Inspired by [rtk](https://github.com/rtk-ai/rtk): one small binary that works as an agent hook, a command proxy or a stdin filter.

## Install & forget

macOS or Linux:

```sh
brew install ludovicl/tap/ranon
ranon init -g       # Claude Code; every other agent: see "Works in your agent"
```

Without Homebrew: a static binary from [Releases](https://github.com/ludovicl/ranon/releases/latest), or `cargo install ranon` (Rust 1.88+).

Restart your agent. That's it: when it runs `psql`, `heroku run` or an MCP tool, it reads tokens instead of names and emails. Nothing to learn, nothing to call.

> `--dry-run` shows the change before writing it, `--uninstall` removes it, and the name dictionary masks names in free text too: **[Getting started](docs/getting-started.md)**

## What the agent reads

| Source | What the agent reads |
|---|---|
| `psql`, `heroku run` console | Names, emails, phones, IBANs, addresses, birth dates as tokens; ids, amounts, statuses and dates readable |
| Stripe CLI, Notion, Slack, Metabase MCP tools | The same, in JSON: a customer's `name` is masked, a product's `name` is not |
| `heroku logs` | Emails and IP addresses in request parameters |
| Data files read by the agent (`.csv`, `.json`, `.log`…), browser pages | The same; source code and screenshots are left as they are |
| Free text (messages, tickets, comments) | Full names of the people in your users table, from a hashed [name dictionary](docs/getting-started.md#name-dictionary-optional), and of the people ranon has already masked in a column (a Slack author) |

Same value, same token: the model can still join, count and compare. Every masked command and field: **[Rules](docs/rules.md)**

**Three promises**

- **Fails closed.** If masking cannot run, the output is hidden, never passed through raw.
- **Stays on your machine.** The token map, its secret salt and the name hashes never leave it. `ranon unmask` turns the model's answer back into real values, locally.
- **Writes back what it masked.** In Claude Code, when the agent writes to Notion or Slack, the tokens become real values again just before the call, and you approve it.

ranon is a filter, not a security boundary: a name typed in a `where` clause, or a field under an unknown name, goes through. What gets past: **[Limits](docs/limits.md)**

## Works in your agent

| Agent | Install | Shell commands | MCP tools |
|---|---|---|---|
| **Claude Code** | `ranon init -g` | hook | hook, with write-back |
| **OpenCode** | `ranon init -g --agent opencode` | PATH shim | **not masked** |
| **Codex CLI** | `ranon init -g --agent codex` | PATH shim | **not masked** |
| **Cursor** | `ranon init -g --agent cursor` | command rewritten | hook |
| Any other agent | `ranon init --agent shims` | PATH shim | **not masked** |

Claude Code and Cursor let a hook mask MCP results. Elsewhere ranon sees shell commands only, so an MCP server that returns personal data reaches the model in clear: use it from Claude Code or Cursor, or leave it off.

> What each agent's hooks allow, and what `ranon init` writes: **[Agents](docs/agents.md)**

## Documentation

| | |
|---|---|
| [Getting started](docs/getting-started.md) | `ranon init` options, name dictionary, `ranon doctor`, a prompt that has your agent check the setup |
| [Agents](docs/agents.md) | Claude Code and Cursor hooks, PATH shims for OpenCode and Codex |
| [How it works](docs/how-it-works.md) | the masking passes, how names are recognized, tokens, local state and `ranon forget` |
| [Rules](docs/rules.md) | built-in rules, your own rules per source, `ranon rules` |
| [Limits](docs/limits.md) | what goes through |

Repository references: [AGENTS.md](AGENTS.md) · [SECURITY.md](SECURITY.md)

## Feedback

Questions, ideas or a command ranon should mask: [open an issue](https://github.com/ludovicl/ranon/issues/new) with synthetic values only. A leak (real data that goes through unmasked) is reported privately: see [SECURITY.md](SECURITY.md).

## Development

```sh
cargo test                                   # unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Integration tests ([`tests/cli.rs`](tests/cli.rs)) run the real binary against synthetic samples in an isolated `RANON_HOME`.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
