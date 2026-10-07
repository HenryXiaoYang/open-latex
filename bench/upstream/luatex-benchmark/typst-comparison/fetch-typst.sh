#!/usr/bin/env bash
# Download official Typst release binaries into $TYPST_VERSIONS_DIR/<version>/typst.
#   ./fetch-typst.sh                  # the versions run.sh uses
#   ./fetch-typst.sh 0.13.1 0.14.2    # specific versions
set -euo pipefail
DIR=${TYPST_VERSIONS_DIR:-$HOME/.cache/typst-versions}
case "$(uname -s) $(uname -m)" in
  "Linux x86_64")   T=x86_64-unknown-linux-musl ;;
  "Linux aarch64")  T=aarch64-unknown-linux-musl ;;
  "Darwin arm64")   T=aarch64-apple-darwin ;;
  "Darwin x86_64")  T=x86_64-apple-darwin ;;
  *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
for v in ${@:-0.12.0 0.13.0 0.13.1 0.14.0 0.14.2 0.15.0 0.15.1}; do
  mkdir -p "$DIR/$v"
  curl -sSL "https://github.com/typst/typst/releases/download/v$v/typst-$T.tar.xz" |
    tar -xJ -C "$DIR/$v" --strip-components=1
  echo "$v: $("$DIR/$v/typst" --version)"
done
