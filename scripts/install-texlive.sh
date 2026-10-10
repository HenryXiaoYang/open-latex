#!/usr/bin/env bash
# Install a minimal TeX Live suitable for rtex (LuaLaTeX + fontspec + microtype + biber).
#
# Usage: scripts/install-texlive.sh [TEXDIR]
# Linux, macOS, and Windows (Git Bash / MSYS2: the GitHub Actions `bash` shell).
#   TL_REPO   override the CTAN tlnet mirror (default: a pinned mirror known to work)
#   TEXDIR    installation directory (default: <repo>/build/texlive)
# Idempotent: re-running only installs missing packages. Writes <repo>/build/texlive.env
# which can be sourced to put lualatex on PATH.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEXDIR="${1:-${TEXDIR:-$REPO_ROOT/build/texlive}}"
TL_REPO="${TL_REPO:-https://mirror.clarkson.edu/ctan/systems/texlive/tlnet}"
PROFILE="$REPO_ROOT/scripts/texlive.profile"
WORK="$REPO_ROOT/build/texlive-installer"

# Collections and packages. collection-luatex brings luaotfload/lualibs/luatexbase;
# collection-latexrecommended brings microtype, geometry, xcolor, booktabs...
PACKAGES=(
  collection-latex collection-latexrecommended collection-luatex
  fontspec unicode-math lm lm-math tex-gyre tex-gyre-math
  lipsum biber bibtex biblatex logreq xstring etoolbox
  pgf hyperref csquotes
  enumitem multirow ulem cancel wrapfig titlesec siunitx algorithms algorithmicx framed soul
  pgfplots circuitikz xecjk xypic gensymb regexpatch haranoaji luatexja
)

WINDOWS=0
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) WINDOWS=1 ;; esac

arch() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)  echo x86_64-linux ;;
    Linux-aarch64) echo aarch64-linux ;;
    Darwin-*)      echo universal-darwin ;;
    MINGW*|MSYS*|CYGWIN*) echo windows ;;
    *) echo "unsupported platform" >&2; exit 1 ;;
  esac
}
# a path for TeX Live and native programs: C:/x/y on Windows, unchanged elsewhere
native() { if [ "$WINDOWS" = 1 ]; then cygpath -m "$1"; else echo "$1"; fi; }
ARCH="$(arch)"
TEXDIR="$(native "$TEXDIR")"
BIN="$TEXDIR/bin/$ARCH"
TLMGR=tlmgr
[ "$WINDOWS" = 1 ] && TLMGR=tlmgr.bat

if [ ! -e "$BIN/$TLMGR" ]; then
  echo "==> Installing TeX Live infrastructure into $TEXDIR from $TL_REPO"
  mkdir -p "$WORK" "$TEXDIR"
  # TEXDIR must be set in the profile; append a copy with the resolved path.
  { cat "$PROFILE"; echo "TEXDIR $TEXDIR"; echo "TEXMFLOCAL $TEXDIR/texmf-local";
    echo "TEXMFSYSVAR $TEXDIR/texmf-var"; echo "TEXMFSYSCONFIG $TEXDIR/texmf-config";
    echo "TEXMFVAR $TEXDIR/texmf-var"; echo "TEXMFCONFIG $TEXDIR/texmf-config";
    echo "TEXMFHOME $TEXDIR/texmf-home"; } > "$WORK/texlive.profile"
  if [ "$WINDOWS" = 1 ]; then
    curl -fsSL --retry 5 --retry-delay 3 -o "$WORK/install-tl.zip" "$TL_REPO/install-tl.zip"
    unzip -q -o "$WORK/install-tl.zip" -d "$WORK"
    INSTALLER_DIR="$(find "$WORK" -maxdepth 1 -type d -name 'install-tl-*' | sort | tail -n1)"
    # the batch file runs the installer with TeX Live's own Perl
    cmd //c "$(cygpath -w "$INSTALLER_DIR/install-tl-windows.bat")" -no-gui \
      -repository "$TL_REPO" -profile "$(cygpath -w "$WORK/texlive.profile")"
  else
    curl -fsSL --retry 5 --retry-delay 3 -o "$WORK/install-tl-unx.tar.gz" "$TL_REPO/install-tl-unx.tar.gz"
    tar -xzf "$WORK/install-tl-unx.tar.gz" -C "$WORK"
    INSTALLER_DIR="$(find "$WORK" -maxdepth 1 -type d -name 'install-tl-*' | sort | tail -n1)"
    perl "$INSTALLER_DIR/install-tl" -repository "$TL_REPO" -profile "$WORK/texlive.profile" -no-gui
  fi
fi

export PATH="$(if [ "$WINDOWS" = 1 ]; then cygpath -u "$BIN"; else echo "$BIN"; fi):$PATH"
echo "==> Installing packages"
"$TLMGR" option repository "$TL_REPO" >/dev/null
"$TLMGR" install "${PACKAGES[@]}"

echo "==> Building formats and font database"
fmtutil-sys --byfmt lualatex >/dev/null 2>&1 || fmtutil-sys --byfmt lualatex
luaotfload-tool --update --force >/dev/null 2>&1 || true

mkdir -p "$REPO_ROOT/build"
cat > "$REPO_ROOT/build/texlive.env" <<ENV
export RTEX_TEXLIVE_BIN="$BIN"
export PATH="$(if [ "$WINDOWS" = 1 ]; then cygpath -u "$BIN"; else echo "$BIN"; fi):\$PATH"
export TEXMFVAR="$TEXDIR/texmf-var"
export TEXMFCONFIG="$TEXDIR/texmf-config"
export TEXMFHOME="$TEXDIR/texmf-home"
ENV
echo "==> Done. lualatex: $(lualatex --version | head -n1)"
echo "    source $REPO_ROOT/build/texlive.env"
