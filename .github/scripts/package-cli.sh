#!/usr/bin/env bash
# Build one release CLI and pack it as a single-file archive under dist/.
#
# Linux musl targets link rustc's bundled musl (`crt-static` is part of
# the target), so the archive does not need a system libc or a cross
# linker. Windows links the CRT statically for the same reason: the exe
# should run on a machine with no toolchain. macOS is pinned to 11, the
# floor of the arm64 target, so the binary is not tied to the runner's
# OS version. Archives are named `pdfmd-<target>.tar.gz` (Windows:
# `.zip`); the README install table has to stay in step with that.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: package-cli.sh <rustc-target>" >&2
    exit 2
fi

target=$1
root=$(unset CDPATH; cd -- "$(dirname "$0")/../.." && pwd)
cd "$root"

case "$target" in
    *-linux-musl)
        # Already the musl default; say it anyway so a future rustc that
        # flips the default still ships a static binary.
        export RUSTFLAGS="-D warnings -C target-feature=+crt-static"
        inner=pdfmd
        archive="pdfmd-${target}.tar.gz"
        ;;
    *-apple-darwin)
        export MACOSX_DEPLOYMENT_TARGET=11.0
        export RUSTFLAGS="-D warnings"
        inner=pdfmd
        archive="pdfmd-${target}.tar.gz"
        ;;
    *-pc-windows-msvc)
        export RUSTFLAGS="-D warnings -C target-feature=+crt-static"
        inner=pdfmd.exe
        archive="pdfmd-${target}.zip"
        ;;
    *)
        echo "unsupported target: $target" >&2
        exit 2
        ;;
esac

export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_INCREMENTAL=0

rustup target add "$target"
cargo build --release --locked --target "$target" --bin pdfmd

bin="target/${target}/release/${inner}"
if [ ! -f "$bin" ]; then
    echo "missing binary: $bin" >&2
    exit 1
fi

case "$target" in
    *-linux-musl)
        if ! command -v file >/dev/null 2>&1; then
            echo "file(1) is required to check the musl binary" >&2
            exit 1
        fi
        desc=$(file "$bin")
        printf '%s\n' "$desc"
        case "$desc" in
            *"statically linked"*|*"static-pie linked"*) ;;
            *)
                echo "musl binary is not statically linked" >&2
                exit 1
                ;;
        esac
        ;;
esac

version_out=$("$bin" --version)
case "$version_out" in
    pdfmd\ *) ;;
    *)
        echo "unexpected --version output: ${version_out}" >&2
        exit 1
        ;;
esac
"$bin" tests/fixtures/sample.pdf >/dev/null

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$bin" "${stage}/${inner}"
mkdir -p dist
dest="${root}/dist/${archive}"
rm -f "$dest"

case "$archive" in
    *.tar.gz)
        tar -C "$stage" -czf "$dest" "$inner"
        listing=$(tar -tzf "$dest")
        ;;
    *.zip)
        if command -v zip >/dev/null 2>&1; then
            (cd "$stage" && zip -q -X "$dest" "$inner")
        else
            # GitHub's Windows runners ship bsdtar, which writes zip for -a.
            tar -C "$stage" -a -cf "$dest" "$inner"
        fi
        if command -v zipinfo >/dev/null 2>&1; then
            listing=$(zipinfo -1 "$dest")
        elif command -v unzip >/dev/null 2>&1; then
            listing=$(unzip -Z1 "$dest")
        else
            listing=$(tar -tf "$dest")
        fi
        ;;
esac

if [ "$listing" != "$inner" ]; then
    echo "archive ${archive} should contain only ${inner}, found:" >&2
    printf '%s\n' "$listing" >&2
    exit 1
fi

printf 'packed %s\n' "$dest"
