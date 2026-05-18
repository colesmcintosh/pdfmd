# Claude Code guide

See [AGENTS.md](AGENTS.md) for the project context, layout, build/test/lint
commands, and the conventions agents are expected to follow here.

In short: zero runtime dependencies, MSRV 1.70, CI runs
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test --all-targets` with `RUSTFLAGS=-D warnings` on Linux, macOS,
and Windows. Coverage sits around 99.75% — new code needs tests.
