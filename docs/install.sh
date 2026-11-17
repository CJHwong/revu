#!/bin/sh
# Installs the latest revu release for this machine.
#   curl -fsSL https://cjhwong.github.io/revu/install.sh | sh
# REVU_INSTALL_DIR picks the folder; the default is ~/.local/bin.
set -eu

repo="CJHwong/revu"
install_dir="${REVU_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    echo "revu install: $1" >&2
    exit 1
}

case "$(uname -s)" in
    Darwin) os="apple-darwin" ;;
    Linux) os="unknown-linux-musl" ;;
    *) fail "revu runs on macOS and Linux, not $(uname -s)." ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch="aarch64" ;;
    x86_64 | amd64) arch="x86_64" ;;
    *) fail "no build for the $(uname -m) processor." ;;
esac
archive="revu-${arch}-${os}.tar.gz"
base="https://github.com/${repo}/releases/latest/download"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q "$1" -O "$2"; }
else
    fail "needs curl or wget to download."
fi
if command -v sha256sum >/dev/null 2>&1; then
    checksum() { sha256sum "$1" | awk '{ print $1 }'; }
elif command -v shasum >/dev/null 2>&1; then
    checksum() { shasum -a 256 "$1" | awk '{ print $1 }'; }
else
    fail "needs sha256sum or shasum to check the download."
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "Downloading ${archive}"
fetch "${base}/${archive}" "${work}/${archive}" || fail "could not download ${base}/${archive}"
fetch "${base}/checksums.txt" "${work}/checksums.txt" || fail "could not download the checksums"

expected=$(awk -v file="$archive" '$2 == file { print $1 }' "${work}/checksums.txt")
[ -n "$expected" ] || fail "checksums.txt has no entry for ${archive}"
actual=$(checksum "${work}/${archive}")
[ "$expected" = "$actual" ] || fail "the download does not match its checksum, so it was not installed."

tar -xzf "${work}/${archive}" -C "$work" revu
mkdir -p "$install_dir"
install -m 755 "${work}/revu" "${install_dir}/revu"
echo "Installed revu to ${install_dir}/revu"

case ":${PATH}:" in
    *":${install_dir}:"*) ;;
    *) echo "Add ${install_dir} to your PATH to run revu from anywhere." ;;
esac
if ! command -v gh >/dev/null 2>&1; then
    echo "revu needs the GitHub CLI: install gh, then run gh auth login."
fi
