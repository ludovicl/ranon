# Contributing

Issues and pull requests are welcome, from humans and coding agents alike.

- **Synthetic data only**, everywhere: issues, tests, fixtures, commit messages. `Jean Dupont`, `jean@example.com`, `FR7630006000011234567890189`.
- **A value that goes through unmasked** is the most useful report: open an issue with a synthetic sample shaped like the real output. If it only shows with real data, report it [privately](SECURITY.md).
- **Before a pull request**, read [AGENTS.md](AGENTS.md): the layout, the invariants (fail closed, stable token format, nothing personal in logs) and how to add a rule. A masking change comes with a test that fails without it.

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same three on Linux and macOS.

By contributing, you agree that your work is licensed under the same terms as ranon ([Business Source License 1.1](LICENSE)), and that Ludovic Lardies may also distribute it under a commercial license.
