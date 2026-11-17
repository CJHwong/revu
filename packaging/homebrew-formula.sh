#!/bin/sh
# Writes the Homebrew formula for a release to stdout.
# Usage: homebrew-formula.sh <version> <checksums.txt>
set -eu

version="$1"
checksums="$2"
base="https://github.com/CJHwong/revu/releases/download/v${version}"

sha() {
    sum=$(awk -v file="revu-$1.tar.gz" '$2 == file { print $1 }' "$checksums")
    if [ -z "$sum" ]; then
        echo "no checksum for revu-$1.tar.gz in $checksums" >&2
        exit 1
    fi
    echo "$sum"
}

cat <<EOF
class Revu < Formula
  desc "Your GitHub pull requests, reviewed in the terminal"
  homepage "https://cjhwong.github.io/revu/"
  version "${version}"
  license "MIT"

  on_macos do
    on_arm do
      url "${base}/revu-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "${base}/revu-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "${base}/revu-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-musl)"
    end
    on_intel do
      url "${base}/revu-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-musl)"
    end
  end

  depends_on "gh"

  def install
    bin.install "revu"
  end

  test do
    assert_predicate bin/"revu", :executable?
  end
end
EOF
