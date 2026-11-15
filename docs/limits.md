# Limits

ranon is a filter, not a security boundary.

- It masks results, not queries: a name you type in a `where` clause is seen by the model.
- Masking depends on names: a field called `name` is only masked where its parent says it is a person. A bare `name` beside an IBAN, in a JSON object or a table, is masked as the account holder. Otherwise a table has no parent, so `select name from beneficiaries`, an alias (`select email as e`), a computed column (`first_name || ' ' || last_name`, `array_agg(last_name)`) or a custom separator (`psql -F ';'`) go through: select the columns under their own names. Notion property names are free text, so add yours to the rules file.
- Values outside the known columns, keys and patterns, names missing from the dictionary and lone first names in free text go through.
- The hook only sees what a tool returns to the agent. Output sent to a file or produced by a background command, then read as a file, is not masked; neither is a tool missing from the hook's matcher. Pipe such output through `ranon mask` before it is written.
- Read masks data files only: personal data in a source file, a Markdown note or a file without extension goes through. Screenshots are never masked.
- Calling the real binary by absolute path bypasses the shim.
- The agent runs as you, so it can read `map.tsv` and unmask everything; under Codex the writable root also lets it change the file. Keeping the state away from the agent takes a separate user or a sandbox that hides the directory.
- For real isolation, give the agent a database role that can only read pseudonymized views.

## Possible next step

A local proxy in front of the model API (`ANTHROPIC_BASE_URL`) would mask everything the model reads, whatever the agent or tool, and put real values back in its tool calls. Not planned: it means an HTTP server, streaming, prompt-cache stability and two API formats, for a gap that only matters outside Claude Code.
