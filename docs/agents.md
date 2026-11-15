# Agents

`ranon init` wires each agent below and keeps what is already in its config. This page shows what it writes, and how to do it by hand.

What ranon can mask depends on what the harness lets a hook do. Only Claude Code lets a hook rewrite any tool output; elsewhere, the PATH shim covers shell commands and MCP output stays in clear.

| Harness | Shell commands | MCP tools | Real values back before a write | Wiring |
|---|---|---|---|---|
| Claude Code | yes, hook | yes, hook | yes, `PreToolUse` | `ranon init`, [details](#claude-code) |
| OpenCode | yes, PATH shim | no | no | `ranon init --agent opencode`, [details](#opencode) |
| Codex CLI | yes, PATH shim | no | no | `ranon init --agent codex`, [details](#codex-cli) |
| Cursor | not supported | not supported | no | [why](#cursor) |

## Claude Code

`ranon init` adds three hooks to `.claude/settings.json` (or `~/.claude/settings.json` with `-g`):

```json
{
  "hooks": {
    "PostToolUse": [
      { "matcher": "Bash|Read|mcp__.*",
        "hooks": [{ "type": "command", "command": "ranon hook claude" }] }
    ],
    "PreToolUse": [
      { "matcher": "mcp__.*__.*(create|update|send|post|comment|reply|write|edit|add|type|fill|form|select|navigate|computer).*",
        "hooks": [{ "type": "command", "command": "ranon hook claude" }] },
      { "matcher": "Bash|Read|mcp__.*",
        "hooks": [{ "type": "command", "command": "command -v ranon >/dev/null 2>&1 || { echo 'ranon is missing: output would not be masked' >&2; exit 2; }" }] }
    ]
  }
}
```

A hook that already runs `ranon hook` keeps its matcher: to mask fewer MCP servers, narrow it by hand (`Bash|mcp__notion__.*`) and `ranon init` will leave it alone.

For Bash, only the outputs of `psql`, `heroku run`, `heroku logs`, `stripe` and the `commands` of your rules files are masked. For Read, only data files (`.csv`, `.tsv`, `.json`, `.jsonl`, `.ndjson`, `.txt`, `.log`, `.xml`, `.html`, `.sql`): masking source code would turn its fixtures into tokens. For any other matched tool, every string of the response is, except screenshots. If masking fails (unusable state, invalid rules file, unreadable payload, internal error), the output is replaced by an error message (fail closed).

**Writing back.** When the model writes to a tool (a Notion comment, a page, a field typed in a browser), its text holds tokens. The `PreToolUse` hook puts the real values back, locally, just before the call, and asks you to approve it since the input changed. Unknown tokens are left as they are.

Two failures are out of ranon's reach, because Claude Code then keeps the raw output: a hook whose binary is missing, and a hook that times out. The third hook covers the first: it blocks Bash and MCP calls while `ranon` is not in `PATH`.

## PATH shim (any agent)

```sh
ranon init --agent shims              # a shim for every masked command
ranon init --agent shims psql heroku  # or for these only
```

`ranon init --agent opencode` and `--agent codex` do this for you. Otherwise, put `~/.local/share/ranon/shims` first in the **agent's** PATH only, not your own shell. The agent keeps typing `psql`; ranon finds the real binary further down the PATH, runs it and masks stdout and stderr, keeping the exit code.

Where that PATH is set depends on the harness.

### OpenCode

`ranon init --agent opencode` creates the shims and writes this plugin to `.opencode/plugins/ranon.ts` (`-g`: `~/.config/opencode/plugins/ranon.ts`). OpenCode hands the environment of every shell command to a plugin, so one file is enough:

```ts
// .opencode/plugins/ranon.ts, or ~/.config/opencode/plugins/ranon.ts for every project
import { existsSync } from "node:fs"
import { homedir } from "node:os"
import { join } from "node:path"

const shims = join(homedir(), ".local", "share", "ranon", "shims")

export default {
  id: "ranon",
  async setup(ctx: any) {
    if (!existsSync(shims)) return
    await ctx.shell.hook("create.before", (event: { env?: Record<string, string | undefined> }) => {
      if (!event.env) return
      const path = event.env.PATH ?? process.env.PATH ?? ""
      if (path.split(":").includes(shims)) return
      event.env.PATH = `${shims}:${path}`
    })
  },
}
```

### Codex CLI

`ranon init --agent codex -g` creates the shims and adds this to `~/.codex/config.toml` (without `-g`: `.codex/config.toml`, read only in a trusted project):

```toml
[shell_environment_policy]
# replaces PATH: write the full value, shims first
set = { PATH = "/home/you/.local/share/ranon/shims:/usr/local/bin:/usr/bin:/bin" }

[sandbox_workspace_write]
# ranon must save its token map, or it hides every output holding a new value
writable_roots = ["/home/you/.local/state/ranon"]
```

`set` replaces the variable rather than prepending to it, so copy your current `PATH` after the shims (`echo "$HOME/.local/share/ranon/shims:$PATH"`). Without the writable root, the default `workspace-write` sandbox stops ranon from saving `map.tsv`: the command then prints `ranon: cannot save token map …, output hidden` instead of its output. That is safe but useless. The sandbox also blocks network access by default, so `psql` or `heroku` need it allowed as well.

Codex hooks cannot stand in for the shim: a `PostToolUse` hook can block a result or add context, but not rewrite it (`updatedMCPToolOutput` is parsed and not supported). MCP output therefore stays in clear. Checked with Codex CLI 0.153.4: through the shims, a table of names and emails reached the model as tokens, and the new values were saved.

### Cursor

Not supported yet. Cursor hooks cannot replace shell output (`afterShellExecution` only observes it), and its docs describe no way to set the PATH of the agent's commands. Two hooks could carry an adapter that does not exist yet: `preToolUse` can rewrite a shell command before it runs (`updated_input`), for instance to pipe it through `ranon mask`, and `postToolUse` can replace the output of a local MCP tool (`updated_mcp_tool_output`). Neither is implemented in ranon.

### Check the wiring

Check it from the agent rather than from your shell: `command -v psql` must print the shim, and a query returning a name must show a token. `ranon doctor` only covers the binary and the Claude Code hooks, so with a shim it says nothing about the wiring.

The shim covers commands, so it also masks a command whose output is redirected to a file. It does not cover an MCP server, whose output never goes through a shell: for those, use the Claude Code hook, or a wrapper that pipes the output through `ranon mask`.
