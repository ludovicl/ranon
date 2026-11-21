# Rules

Built-in rules are grouped by source in [`src/rules/builtin.rs`](../src/rules/builtin.rs):

| Set | Covers |
|---|---|
| `common` | names, email, phone, birth date and place, address, nationality, social security and tax numbers, organisation names |
| `payments` | IBAN, creditor/debtor/recipient/cardholder names, card embossed names, Stripe `customer`, `billing_details`, `shipping`, `individual`; command `stripe` |
| `collaboration` | `user.name` in Notion and GitHub user objects, Slack `real_name`, `display_name`, `userName` |
| `security` | IP addresses, passwords, API keys and tokens |
| `databases` | commands `psql`, `heroku run`, `heroku logs` |

## Your own rules

Add yours in `~/.config/ranon/` (or `$XDG_CONFIG_HOME/ranon`, or `$RANON_CONFIG_DIR`), no rebuild needed: `rules.toml`, and any number of `rules.d/*.toml`, e.g. one file per source:

```text
~/.config/ranon/rules.d/
├── notion.toml     # property names of your workspace
├── treezor.toml    # fields of your banking provider
└── gmail.toml      # commands = ["composio"]
```

Each file can hold five kinds of entries:

```toml
# column or key names → kind; user rules are checked before the built-in ones
[[column]]
pattern = '^(nom|prénom)$'
kind = "PERSON"

[[column]]
pattern = '(^|\.)beneficiaries\.name$'      # JSON: "name" inside a "beneficiaries" list
kind = "PERSON"

# values anywhere in the text
[[value]]
pattern = '\b\d{14}\b'
kind = "SIRET"

# keep these visible even if a built-in rule matches
keep = ['^email_id$']

# extra shell commands masked by the Claude Code hook
commands = ["composio"]

# turn off the built-in rules of a kind, columns and value patterns alike
disable = ["IP"]
```

Patterns are [Rust regexes](https://docs.rs/regex) matched on the lowercased name and on its snake_case form; kinds are uppercase letters. The first match wins, in this order: your `keep`, your `[[column]]` rules, the built-in keeps, the built-in columns. `ranon rules` lists every source, and tells which rule applies to a name and where it comes from:

```text
$ ranon rules Prénom creditor_name status
user file: ~/.config/ranon/rules.d/notion.toml
  notion.toml: 4 column rules
  built-in payments: 4 column rules, commands: stripe
  …
Prénom: PERSON (notion.toml)
creditor_name: PERSON (built-in payments)
status: kept visible
```

An invalid file is an error, never a silent fallback: the hook then hides the output.

## Turning a kind off

`disable = ["IP"]` turns off every built-in rule of that kind, for a one-off analysis where you accept that the model reads those values: IP addresses in logs, say. Your own `[[column]]` and `[[value]]` rules of that kind still apply, and `ranon rules` lists what is disabled and by which file. A column can still fall under another kind: with `IP` off, `ip_address` is masked as an `ADDRESS`. Remove the line, or the file, when the analysis is done.
