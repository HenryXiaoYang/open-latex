#!/usr/bin/env bash
# Package a release build into dist/: rtex-<target>.tar.gz (zip on Windows) and its .sha256.
#
#   cargo build --profile dist --locked --target <target> -p rtex-cli -p rtex-core
#   scripts/package.sh <target>
#
# Layout (the rtex binary finds its support files at ../share/rtex/tex):
#   rtex-<version>-<target>/
#     bin/rtex[.exe]
#     lib/        the shared C library for embedding (build the static one from source)
#     include/rtex.h
#     share/rtex/tex/   rtex's TeX and Lua files
#     LICENSE README.md CHANGELOG.md
set -euo pipefail
cd "$(dirname "$0")/.."
target="${1:?usage: scripts/package.sh <target>}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
rel="target/$target/dist"
name="rtex-$version-$target"
stage="dist/$name"
rm -rf "$stage" && mkdir -p "$stage"/{bin,lib,include,share/rtex}

case "$target" in
  *windows*)
    cp "$rel/rtex.exe" "$stage/bin/"
    cp "$rel/rtex_core.dll" "$rel/rtex_core.dll.lib" "$stage/lib/" ;;
  *apple*)
    cp "$rel/rtex" "$stage/bin/"
    cp "$rel/librtex_core.dylib" "$stage/lib/" ;;
  *)
    cp "$rel/rtex" "$stage/bin/"
    cp "$rel/librtex_core.so" "$stage/lib/" ;;
esac
cp include/rtex.h "$stage/include/"
# support files only: the experiments and the Lua tests are not needed at run time
mkdir -p "$stage/share/rtex/tex"
(cd tex && find . -type f ! -path './experiments/*' ! -path './tests/*' -print) | while read -r f; do
  mkdir -p "$stage/share/rtex/tex/$(dirname "$f")"
  cp "tex/$f" "$stage/share/rtex/tex/$f"
done
cp LICENSE README.md CHANGELOG.md "$stage/"

sha() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
cd dist
case "$target" in
  *windows*) archive="rtex-$target.zip"; rm -f "$archive"; 7z a -tzip -bd -bso0 "$archive" "$name" ;;
  *)         archive="rtex-$target.tar.gz"; tar -czf "$archive" "$name" ;;
esac
sha "$archive" > "$archive.sha256"
echo "dist/$archive"
