#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=0.2.0
target=${EGGTUNNEL_TEST_TARGET:-aarch64-apple-darwin}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/eggtunnel-install-test.XXXXXX")
trap 'rm -rf "$tmp" "$root/THIRD_PARTY_NOTICES.md"' EXIT HUP INT TERM

cd "$root"
cargo build --locked --release -p eggtunnel-cli
python3 scripts/generate-third-party-notices.py
release="$tmp/release/v$version"
stage="$tmp/stage"
mkdir -p "$release" "$stage"
cp target/release/eggtunnel LICENSE-MIT THIRD_PARTY_NOTICES.md "$stage/"
printf '%s\n' "$version" > "$stage/VERSION"
archive="eggtunnel-v$version-$target.tar.gz"
tar -C "$stage" -czf "$release/$archive" .
(cd "$release" && shasum -a 256 "$archive" > SHA256SUMS)

EGGTUNNEL_RELEASE_BASE_URL="file://$tmp/release" \
EGGTUNNEL_TARGET="$target" \
    "$root/install.sh" "v$version" "$tmp/custom-bin"
"$tmp/custom-bin/eggtunnel" version | grep -F "eggtunnel $version"

printf x >> "$release/$archive"
if EGGTUNNEL_RELEASE_BASE_URL="file://$tmp/release" \
    EGGTUNNEL_TARGET="$target" "$root/install.sh" "v$version" "$tmp/bad-bin"; then
    echo "installer accepted a corrupted archive" >&2
    exit 1
fi

if EGGTUNNEL_TARGET="unsupported-target" "$root/install.sh" "v$version" "$tmp/unsupported-bin"; then
    echo "installer accepted an unsupported target" >&2
    exit 1
fi
