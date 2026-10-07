#!/usr/bin/env bash
# Every function declared in include/lode.h must be exported by the shared library.
set -euo pipefail
cd "$(dirname "$0")/.."
lib=target/release/liblode_core.so
[ -f "$lib" ] || { echo "build first: cargo build --release -p lode-core"; exit 2; }
missing=0
for fn in $(grep -oE '\blode_[a-z_0-9]+\s*\(' include/lode.h | sed 's/\s*(//' | sort -u); do
  if ! nm -D --defined-only "$lib" | grep -q " T $fn\$"; then echo "missing export: $fn"; missing=1; fi
done
for fn in $(nm -D --defined-only "$lib" | awk '{print $3}' | grep '^lode_' | sort -u); do
  grep -q "\b$fn\b" include/lode.h || echo "exported but not in header: $fn"
done
[ $missing -eq 0 ] && echo "ABI header matches exports"
exit $missing
