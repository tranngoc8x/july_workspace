#!/usr/bin/env bash
# Build release binaries for the macOS targets used in development and emit
# tarballs plus SHA256SUMS under dist/. Targets may be overridden as arguments.
set -euo pipefail

cd "$(dirname "$0")/.."

version=$(sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "cannot read package version from Cargo.toml" >&2; exit 1; }

if [ "$#" -gt 0 ]; then
  targets=("$@")
else
  targets=(aarch64-apple-darwin x86_64-apple-darwin)
fi

host=$(rustc -vV | sed -n 's/^host: //p')
dist=dist
rm -rf "$dist"
mkdir -p "$dist"

for target in "${targets[@]}"; do
  if ! rustup target list --installed | grep -qx "$target"; then
    echo "missing target $target; run: rustup target add $target" >&2
    exit 1
  fi
  cargo build --release --target "$target"
  binary="target/$target/release/july"
  # ponytail: only the host build is smoke-tested; cross builds cannot run here.
  if [ "$target" = "$host" ]; then
    "$binary" --version
  fi
  tar -czf "$dist/july-$version-$target.tar.gz" -C "$(dirname "$binary")" july
done

(cd "$dist" && shasum -a 256 ./*.tar.gz >SHA256SUMS)
cat "$dist/SHA256SUMS"
