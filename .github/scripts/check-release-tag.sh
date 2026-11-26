#!/bin/sh
# A release tag is `v` plus the Cargo.toml version, nothing else.
# `cargo publish` ships that version string, so a mismatched tag would
# attach binaries for one version and upload a different crate.
set -eu

ref=${1:-${GITHUB_REF_NAME:-}}
if [ -z "$ref" ]; then
    echo "usage: check-release-tag.sh vX.Y.Z  (or set GITHUB_REF_NAME)" >&2
    exit 2
fi

root=$(unset CDPATH; cd -- "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
if [ -z "$version" ]; then
    echo "could not read version from $root/Cargo.toml" >&2
    exit 1
fi

expect="v${version}"
if [ "$ref" != "$expect" ]; then
    echo "tag ${ref} does not match Cargo.toml version ${version} (expected ${expect})" >&2
    exit 1
fi

printf 'tag %s matches Cargo.toml\n' "$ref"
