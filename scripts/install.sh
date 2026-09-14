#!/usr/bin/env bash
# Build and install the july release binary. Workspace data in ~/.july is
# never touched: installing over an existing binary only replaces the binary.
set -euo pipefail

cd "$(dirname "$0")/.."

prefix="${JULY_PREFIX:-$HOME/.local}"
if [ "$#" -gt 0 ]; then
  case "$1" in
    --prefix) prefix="${2:?--prefix needs a directory}" ;;
    *) echo "usage: install.sh [--prefix DIR]" >&2; exit 1 ;;
  esac
fi

cargo build --release
mkdir -p "$prefix/bin"
install -m 755 target/release/july "$prefix/bin/july"
"$prefix/bin/july" --version
# Bind standalone update permission to this exact path and installed content.
installed_path="$(cd "$prefix/bin" && pwd -P)/july"
receipt="$(mktemp "$prefix/bin/.july-receipt.XXXXXX")"
trap 'rm -f "$receipt"' EXIT
printf '%s\n%s\n' "$installed_path" "$(shasum -a 256 "$installed_path" | awk '{print $1}')" > "$receipt"
mv -f "$receipt" "$prefix/bin/.july-install-receipt"

case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) echo "note: $prefix/bin is not on PATH" >&2 ;;
esac
