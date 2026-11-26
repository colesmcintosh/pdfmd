# Contributing

Development commands, layout, and review rules live in [AGENTS.md](AGENTS.md).
`[dependencies]` stays empty, the MSRV is Rust 1.70, and every change
keeps `cargo fmt`, `cargo clippy`, and `cargo test` green. The CI
commands are in [AGENTS.md](AGENTS.md).

`profile` and `opendataloader` are dev harnesses behind the
`dev-harness` feature, so `cargo install` does not put them on `PATH`:

```sh
cargo run --release --bin profile --features dev-harness
```

## Releasing

Releases are git tags. [`.github/workflows/release.yml`](.github/workflows/release.yml)
runs on `v*` and does two independent jobs: build the CLI archives, and
`cargo publish` the crate. The tag has to be on a commit that already
contains that workflow. GitHub reads the workflow from the tagged
commit, not from `main` after the fact.

`v0.3.0` was tagged before this workflow existed. Do not move that tag.
Bump `version` and tag the new value.

1. Bump `version` in `Cargo.toml`. The Python package reads that field;
   do not keep a second copy.
2. Open a PR and merge it to `main`. `cargo package --locked` in CI
   checks the crate tarball. `cargo publish --dry-run --locked` is the
   same check plus the registry, and it starts failing once that exact
   version is already on crates.io, so it is not a standing CI step.
3. On the merge commit:

   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   The tag is `v` plus the `version` string (`v0.4.0` for `0.4.0`,
   `v0.4.0-rc.1` for `0.4.0-rc.1`). Anything else fails before upload.
   A pre-release version is marked as a GitHub pre-release when this
   workflow creates the release.
4. Optional: create the GitHub release yourself first if the notes
   should be hand-written, which is how v0.1.0–v0.3.0 were cut. The
   workflow uploads the archives and `SHA256SUMS` onto that release
   and does not replace the body. If no release exists yet, it creates
   one from [`.github/release-notes.md`](.github/release-notes.md).

### crates.io

`publish` runs `cargo publish --locked` with the `CARGO_REGISTRY_TOKEN`
repository secret (Settings → Secrets and variables → Actions).

The first publish needs a crates.io token that is allowed to create a
new crate (`publish-new`). A token scoped to the `pdfmd` crate can only
be created after that crate exists; later releases can use one. The
workflow passes the token in the environment, not on the command line.

If the secret is missing or crates.io rejects it, that job fails and
the workflow run is red. The binary jobs do not depend on it, so the
GitHub release still receives its archives. Fix the token and re-run
the `publish` job. Re-running the whole workflow uploads the same
archives again (`--clobber`) and `cargo publish` fails if that version
is already on crates.io.

### Binaries

| archive | runner |
| --- | --- |
| `pdfmd-x86_64-unknown-linux-musl.tar.gz` | `ubuntu-latest` |
| `pdfmd-aarch64-unknown-linux-musl.tar.gz` | `ubuntu-24.04-arm` |
| `pdfmd-x86_64-apple-darwin.tar.gz` | `macos-latest` (cross-compile) |
| `pdfmd-aarch64-apple-darwin.tar.gz` | `macos-latest` |
| `pdfmd-x86_64-pc-windows-msvc.zip` | `windows-latest` |

Each Unix archive contains one `pdfmd` binary. The zip contains
`pdfmd.exe`. `SHA256SUMS` is the `sha256sum` output for those five
files. Linux binaries are static musl. macOS targets 11 or newer.
The Windows build links the CRT statically. Nothing is code-signed.

The release is built with the runner's stable toolchain, not with the
1.70 MSRV. 1.70 is the floor for people compiling from source.

### PyPI

Do not publish the Python package. The `pdfmd` name on PyPI belongs to
an unrelated project. Bindings stay `pip install ./python` from a
checkout.
