#!/bin/sh
set -eu

usage() {
    echo "usage: install.sh <version> [destination] [target]" >&2
    echo "target defaults to the current Linux/macOS architecture" >&2
    exit 2
}

[ "$#" -ge 1 ] && [ "$#" -le 3 ] || usage
version=$1
destination=${2:-${EGGTUNNEL_INSTALL_DIR:-"$HOME/.local/bin"}}
target_override=${3:-${EGGTUNNEL_TARGET:-}}
case "$version" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) echo "version must use a vMAJOR.MINOR.PATCH tag" >&2; exit 2 ;;
esac

if [ -n "$target_override" ]; then
    target=$target_override
else
    os=$(uname -s)
    arch=$(uname -m)
    case "$os/$arch" in
        Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
        Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-gnu ;;
        Darwin/x86_64) target=x86_64-apple-darwin ;;
        Darwin/arm64|Darwin/aarch64) target=aarch64-apple-darwin ;;
        *) echo "unsupported target: $os/$arch" >&2; exit 1 ;;
    esac
fi
case "$target" in
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin) ;;
    *) echo "unsupported target triple: $target" >&2; exit 1 ;;
esac

archive="eggtunnel-${version}-${target}.tar.gz"
base=${EGGTUNNEL_RELEASE_BASE_URL:-"https://github.com/eggstack/eggtunnel/releases/download"}
release_url=${base%/}/$version
tmp=$(mktemp -d "${TMPDIR:-/tmp}/eggtunnel-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

curl --fail --location --silent --show-error "$release_url/$archive" -o "$tmp/$archive"
curl --fail --location --silent --show-error "$release_url/SHA256SUMS" -o "$tmp/SHA256SUMS"
expected=$(awk -v asset="$archive" '$2 == asset { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || { echo "checksum entry missing for $archive" >&2; exit 1; }
case "$(uname -s)" in
    Darwin) actual=$(shasum -a 256 "$tmp/$archive" | awk '{print $1}') ;;
    *) actual=$(sha256sum "$tmp/$archive" | awk '{print $1}') ;;
esac
[ "$expected" = "$actual" ] || { echo "checksum verification failed for $archive" >&2; exit 1; }

mkdir "$tmp/unpacked"
tar -xzf "$tmp/$archive" -C "$tmp/unpacked"
[ -x "$tmp/unpacked/eggtunnel" ] || { echo "archive is missing executable eggtunnel" >&2; exit 1; }
[ -f "$tmp/unpacked/VERSION" ] || { echo "archive is missing VERSION" >&2; exit 1; }
[ "$(cat "$tmp/unpacked/VERSION")" = "${version#v}" ] || { echo "archive version does not match requested version" >&2; exit 1; }
mkdir -p "$destination"
install -m 755 "$tmp/unpacked/eggtunnel" "$destination/eggtunnel"
printf 'installed Eggtunnel %s to %s/eggtunnel\n' "${version#v}" "$destination"
