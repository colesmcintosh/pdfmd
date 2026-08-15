<!--
Read VISION.md and AGENTS.md first.
Title: `module: lowercase summary` (e.g. `pdf: cap dictionary entry count`).
-->

## Summary

<!-- What changed and why. A few sentences, not a file list. -->

## Module

<!-- pdf / extract / heuristics / cli / docs / ci -->

## Checklist

- [ ] `[dependencies]` in `Cargo.toml` is still empty
- [ ] MSRV 1.70 — no language or std features added after 1.70
- [ ] Tests live next to the code (`#[cfg(test)] mod tests`); `tests/integration.rs` only if a real PDF or the CLI binary is required
- [ ] `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets` pass
- [ ] Hot path not regressed (`deflate`, content-stream tokenizer, `parallel_map`); profiled if those files were touched
- [ ] Still one error type: `pdf::PdfError`
- [ ] No encryption or `LZWDecode` stubs
- [ ] First-class docs (`README.md`, `VISION.md`, `AGENTS.md`, `CLAUDE.md`) updated if behavior or rules changed

## Notes

<!-- Fixture, benchmark, or follow-up. Delete this section if none. -->
