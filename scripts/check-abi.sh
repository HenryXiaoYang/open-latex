#!/usr/bin/env bash
# Every function declared in include/rtex.h must be exported by the shared library.
set -euo pipefail
cd "$(dirname "$0")/.."
case "$(uname -s)" in
  Darwin)
    lib=target/release/librtex_core.dylib
    # Mach-O symbols carry a leading underscore
    exports() { nm -gU "$lib" | awk '$2 == "T" {print $3}' | sed 's/^_//'; } ;;
  *)
    lib=target/release/librtex_core.so
    exports() { nm -D --defined-only "$lib" | awk '$2 == "T" {print $3}'; } ;;
esac
[ -f "$lib" ] || { echo "build first: cargo build --release -p rtex-core"; exit 2; }
syms="$(exports | grep '^rtex_' | sort -u)"
missing=0
# POSIX classes only: BSD grep/sed (macOS) have no \b or \s
for fn in $(grep -oE 'rtex_[a-z_0-9]+[[:space:]]*\(' include/rtex.h | sed 's/[[:space:]]*(//' | sort -u); do
  if ! grep -qx "$fn" <<<"$syms"; then echo "missing export: $fn"; missing=1; fi
done
for fn in $syms; do
  grep -qw "$fn" include/rtex.h || echo "exported but not in header: $fn"
done
[ $missing -eq 0 ] && echo "ABI header matches exports"
exit $missing
