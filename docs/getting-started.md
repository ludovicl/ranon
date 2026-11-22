# Getting started

```sh
brew install ludovicl/tap/ranon  # or a binary from GitHub Releases
ranon init -g                    # Claude Code, every project
ranon init -g --agent opencode   # or OpenCode
ranon init -g --agent codex      # or Codex CLI, from your own terminal
```

Without `-g`, `ranon init` configures the current project only. `--dry-run` prints the diff without writing it, `--show` lists where ranon is wired, `--uninstall` removes ranon's entries and nothing else. Existing settings are kept, and running it twice changes nothing. Then restart the agent session.

## Name dictionary (optional)

```sh
export DATABASE_URL=postgres://readonly@replica/app
ranon dict build --sql "select first_name from users union select last_name from users"
# or from any source, one or more names per line:
cut -d, -f2,3 people.csv | ranon dict build --stdin
ranon dict stats
```

Names go straight from `psql` into the hasher; they are never printed. `--db-url-env` reads the URL from another variable, `--replace` starts from an empty dictionary. Without a dictionary, ranon still learns the names it masks in columns (a Slack author, a `first_name`) and masks them in free text afterwards.

## Check the setup

```text
$ ranon doctor
ok    binary: ~/.cargo/bin/ranon
ok    rules: loaded from ~/.config/ranon
ok    state: ~/.local/state/ranon is private
ok    token map: 75 real values kept in map.tsv (`ranon forget` drops them)
ok    name dictionary: 179562 hashed words
ok    self-test: sample table masked
ok    Claude Code hook in .claude/settings.json: masks Bash|mcp__notion__.*
ok    hook activity: 42 outputs masked (318 values), 1 hidden
```

It exits with 1 when a check fails. A crash of the hook in the last 7 days is a warning with the place in ranon's code where it happened (`src/mask.rs:52`): the outputs it hid were replaced by an error, so update ranon or report that location. Run it from the project whose `.claude/settings.json` holds the hooks.

## Let your agent set it up

Paste this prompt into Claude Code, at the root of the project where the agent works. It installs ranon, wires the hooks and checks the result without the agent reading any personal data. Fill in the three values on the first lines.

````text
Install and configure ranon (https://github.com/ludovicl/ranon) in this project.
It masks personal data in tool output before you see it.

My setup:
- Read-only database URL: in the environment variable <NAME OF THE VARIABLE>
- Query listing the names of my users: <e.g. select first_name from users union select last_name from users>
- Tools that return personal data here: <e.g. psql, heroku run, the Notion and Slack MCP servers, the ntn CLI>

Rules for this whole task:
- Never print, read or copy a real name, email or other personal value. Do not
  run a query that returns personal data until step 5 says the hook is active.
- Never open, print or commit the files of ~/.local/state/ranon (salt, map.tsv,
  names.bin). Never print the database URL.
- Do not commit or push anything. Show me the diff of every file you change.
- Stop and ask me if a step fails; do not work around it.

Steps:
1. Read the README of the repository, then install the binary:
   brew install ludovicl/tap/ranon
   Check that `ranon --version` works.
2. Build the name dictionary with the variable and the query above:
   ranon dict build --db-url-env <VARIABLE> --sql "<QUERY>"
   The names go straight into a hasher. Report only the count from `ranon dict stats`.
3. For each CLI of my list that ranon does not mask by default (it masks psql,
   heroku run, heroku logs and stripe), add it to ~/.config/ranon/rules.d/<source>.toml:
   commands = ["<cli>"]
4. Run `ranon init --dry-run`, show me the change, then `ranon init`. It adds the
   masking, write-back and missing-binary hooks to .claude/settings.json and
   keeps everything already there.
5. Ask me to restart the session so the hooks load. Then run `ranon doctor` from
   the project root: every line must be `ok`.
6. Test on real output, one source at a time. Select a single row by ID, with the
   columns under their own names. In each result, list the fields that are still
   readable and look personal, by field name only. For each one, propose a
   [[column]] rule for ~/.config/ranon/rules.d/, check it with `ranon rules <name>`
   and run the same call again.
7. Finish with a summary: sources tested, rules added, what stays unmasked (see
   docs/limits.md in the repository), and the line of `ranon doctor` on hook
   activity.

From now on in this project: filter by ID rather than by name, since ranon masks
results and not the queries you write; run one query per command; and never send
the output of a masked command to a file or to the background, where the hook
does not see it.
````

Step 6 is where a setup gets safe: field names differ in every company, and only real output shows the ones the built-in rules miss. The agent will see the values of the fields that are not covered yet, so start with a single row.

On another harness, step 4 becomes `ranon init --agent opencode` or `--agent codex`: a PATH shim replaces the hook (see [Agents](agents.md)), and no `PreToolUse` pass puts real values back before a write. The rest of the prompt holds as it is.

## Manual use

```sh
ranon psql "$DATABASE_URL" -c "select …"   # proxy
some-command | ranon mask
pbpaste | ranon unmask                     # restore real values, locally
```
