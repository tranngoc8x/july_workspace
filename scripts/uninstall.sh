#!/usr/bin/env bash
# Remove the july binary. Workspace data in ~/.july survives by default and is
# only deleted with an explicit --purge plus an interactive confirmation.
set -euo pipefail

prefix="${JULY_PREFIX:-$HOME/.local}"
data="${JULY_DATA_DIR:-$HOME/.july}"
purge=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
    --purge) purge=true; shift ;;
    *) echo "usage: uninstall.sh [--prefix DIR] [--purge]" >&2; exit 1 ;;
  esac
done

binary="$prefix/bin/july"
if [ -e "$binary" ]; then
  rm -f "$binary"
  echo "removed $binary"
else
  echo "no binary at $binary"
fi

if [ "$purge" != true ]; then
  if [ -d "$data" ]; then
    echo "workspace data kept at $data (delete with --purge)"
  fi
  exit 0
fi

if [ ! -d "$data" ]; then
  echo "no workspace data at $data"
  exit 0
fi
if [ ! -t 0 ]; then
  echo "--purge needs an interactive terminal to confirm" >&2
  exit 1
fi
printf 'delete workspace data at %s? this cannot be undone [y/N] ' "$data"
read -r answer
case "$answer" in
  y|Y) rm -rf "$data"; echo "removed $data" ;;
  *) echo "kept $data" ;;
esac
