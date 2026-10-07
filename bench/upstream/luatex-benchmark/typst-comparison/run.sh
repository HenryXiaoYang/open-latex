#!/usr/bin/env bash
# Typst cross-version comparison: runs bench.mjs against several Typst
# versions on the same document.
#
#   ./fetch-typst.sh              # binaries into $TYPST_VERSIONS_DIR
#   ./run.sh sweep                # 3 round-robin cycles at 300 p, then 10 p / 100 p
#   ./run.sh tags                 # 300 p, tagged vs --no-pdf-tags, 2 cycles
#   ./run.sh split                # 300 p, full export vs --pages 1 (≈ compile only), 2 cycles
#
# Versions are round-robined within each cycle so machine load can't pose as
# a version effect. Runs are strictly sequential (bench.mjs writes
# doc.typ/doc.pdf next to itself) in a temp dir; one summary line per run on
# stdout, full per-run output appended to $LOG.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
DIR=${TYPST_VERSIONS_DIR:-$HOME/.cache/typst-versions}
LOG=${LOG:-/dev/null}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$HERE/bench.mjs" "$WORK/"

load() { uptime | sed -E 's/.*load averages?: ([0-9.]+).*/\1/'; }

run() { # pages cycle version extra
  local out
  out=$(cd "$WORK" && PATH="$DIR/$3:$PATH" TYPST_EXTRA="$4" node bench.mjs "$1" 30 2>&1)
  printf '### p=%s c=%s v=%s extra=%s load=%s\n%s\n' "$1" "$2" "$3" "$4" "$(load)" "$out" >> "$LOG"
  echo "p=$1 c=$2 v=$3 extra='$4' load=$(load) $(grep -E 'Typst version|Document size|Cold compile \(avg\)|Incremental \((avg|p50)\)' <<<"$out" | tr -s ' ' | tr '\n' ' ')"
}

ALL="0.12.0 0.13.0 0.13.1 0.14.0 0.14.2 0.15.0 0.15.1"
case "${1:-}" in
  sweep)
    for c in 1 2 3; do for v in $ALL; do run 300 $c $v ""; done; done
    for p in 10 100; do for v in $ALL; do run $p 1 $v ""; done; done ;;
  tags)
    for c in 1 2; do
      run 300 $c 0.13.1 ""
      for v in 0.14.2 0.15.1; do run 300 $c $v ""; run 300 $c $v "--no-pdf-tags"; done
    done ;;
  split)
    for c in 1 2; do for v in 0.12.0 0.13.1 0.14.2 0.15.1; do
      run 300 $c $v ""; run 300 $c $v "--pages 1"
    done; done ;;
  *) echo "usage: $0 sweep|tags|split" >&2; exit 2 ;;
esac
