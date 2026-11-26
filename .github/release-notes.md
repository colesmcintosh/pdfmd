## Install

From crates.io, with Rust 1.70 or newer:

```sh
cargo install pdfmd
```

Prebuilt CLI binaries and `SHA256SUMS` are attached. Linux builds are
static musl executables (`x86_64` and `aarch64`). macOS builds target
11 or newer (`x86_64` and `arm64`). The Windows `x86_64` build links
the CRT statically. The binaries are unsigned; check `SHA256SUMS`
before copying one onto a machine.

The Python package is not published. The `pdfmd` name on PyPI belongs
to an unrelated project. Install the bindings from a checkout with
`pip install ./python`.
