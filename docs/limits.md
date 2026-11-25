# Limits

ranon is a filter, not a security boundary.

- It masks results, not queries: a name you type in a `where` clause is seen by the model.
- Masking depends on names: a field called `name` is only masked where its parent says it is a person. A bare `name` beside an IBAN, in a JSON object or a table, is masked as the account holder. Otherwise a table has no parent, so `select name from beneficiaries`, an alias (`select email as e`), a computed column (`first_name || ' ' || last_name`, `array_agg(last_name)`) or a custom separator (`psql -F ';'`) go through: select the columns under their own names. Notion property names are free text, so add yours to the rules file.
- Values outside the known columns, keys and patterns go through. In free text, that means postal addresses, birth dates, phone numbers outside France written without their `+` prefix (`(415) 555-0132`), and personal data encoded in base64 other than a JWT.
- Names in free text are masked only as two capitalized dictionary words (`Jean Dupont`) or after a title (`M. Dupont`): a lone first name or surname, a name in lowercase (`jean dupont`) and the `Dupont, Jean` order go through. The dictionary holds words like *Paris* or *Martin*, so loosening this would mask ordinary text.
- What ranon masks in free text depends on what it has already seen: a name it has masked in a column is then recognized in prose, so the same output can be masked differently on a fresh state. `ranon dict build` closes most of the gap from the start.
- Pseudonyms hide values, not the records around them. Ids, cities, amounts and dates stay readable on purpose, and together they can sometimes single a person out.
- The hook only sees what a tool returns to the agent. Output sent to a file or produced by a background command, then read as a file, is not masked; neither is a tool missing from the hook's matcher. Pipe such output through `ranon mask` before it is written.
- Content an MCP server hands over outside a tool call is not masked: skills served by MCP servers (Claude Code 2.1.332 and later) and MCP events that start an OpenCode automation (OpenCode 2.4 and later). Skills are instructions, so the risk is low; an event payload can hold customer data, so turn automations off with `"automations": false` in the OpenCode config if your MCP servers push events.
- Read masks data files only: personal data in a source file, a Markdown note or a file without extension goes through. Screenshots are never masked.
- Calling the real binary by absolute path bypasses the shim.
- The agent runs as you, so it can read `map.tsv` and unmask everything; under Codex the writable root also lets it change the file. Keeping the state away from the agent takes a separate user or a sandbox that hides the directory.
- For real isolation, give the agent a database role that can only read pseudonymized views.

## Possible next step

A local proxy in front of the model API (`ANTHROPIC_BASE_URL`) would mask everything the model reads, whatever the agent or tool, and put real values back in its tool calls. Not planned: it means an HTTP server, streaming, prompt-cache stability and two API formats, for a gap that only matters outside Claude Code.
