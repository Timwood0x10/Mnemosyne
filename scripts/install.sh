#!/usr/bin/env bash
#
# Mnemosyne installer — download the prebuilt binary for the current
# platform/architecture and unpack it into a `.mnemosyne/` directory.
#
# The release archives are produced by `.github/workflows/release.yml` and
# contain three files: the `mnemosyne` binary, `markers_zh.json` and
# `markers_en.json` (the customizable observation-marker word lists). The
# binary resolves these JSON files from its own directory, so installing
# everything into one `.mnemosyne/` directory works out of the box.
#
# Usage:
#   ./install.sh                 # install the latest release
#   ./install.sh v0.1.3          # install a specific release tag
#   MNEMOSYNE_HOME=/opt/mnemosyne ./install.sh   # custom install directory
#
# Install directory: $MNEMOSYNE_HOME if set, else ~/.mnemosyne.

set -euo pipefail

REPO="Timwood0x10/Mnemosyne"
TAG="${1:-latest}"
# Default install location; honor an explicit MNEMOSYNE_HOME (the same env
# var the binary uses as its resource root at runtime).
INSTALL_DIR="${MNEMOSYNE_HOME:-$HOME/.mnemosyne}"

# --- 1. Detect the platform (uname -s) --------------------------------
case "$(uname -s)" in
  Darwin)            OS="apple-darwin" ;;
  Linux)             OS="unknown-linux-gnu" ;;
  MINGW* | MSYS* | CYGWIN*)
                     OS="pc-windows-msvc" ;;
  *)
    echo "error: unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

# --- 2. Detect the architecture (uname -m) ----------------------------
case "$(uname -m)" in
  arm64 | aarch64)   ARCH="aarch64" ;;
  x86_64 | amd64)    ARCH="x86_64" ;;
  *)
    echo "error: unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

TARGET="${ARCH}-${OS}"
BINARY="mnemosyne"
if [ "$OS" = "pc-windows-msvc" ]; then
  BINARY="mnemosyne.exe"
fi

# --- 3. Download the matching release archive --------------------------
# Latest:   .../releases/latest/download/<archive>
# Tagged:   .../releases/download/<tag>/<archive>
ARCHIVE="mnemosyne-${TARGET}.tar.gz"
if [ "$TAG" = "latest" ]; then
  URL="https://github.com/${REPO}/releases/latest/download/${ARCHIVE}"
else
  URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE}"
fi

echo "Installing Mnemosyne ${TAG} for ${TARGET} → ${INSTALL_DIR}"
echo "Downloading ${URL} ..."

TMP_ARCHIVE="$(mktemp)"
trap 'rm -f "$TMP_ARCHIVE"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fsSL -o "$TMP_ARCHIVE" "$URL"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TMP_ARCHIVE" "$URL"
else
  echo "error: neither curl nor wget is available" >&2
  exit 1
fi

# --- 4. Create .mnemosyne/ and extract --------------------------------
mkdir -p "$INSTALL_DIR"
tar -xzf "$TMP_ARCHIVE" -C "$INSTALL_DIR"
chmod +x "${INSTALL_DIR}/${BINARY}"

echo
echo "Installed:"
echo "  ${INSTALL_DIR}/${BINARY}"
echo "  ${INSTALL_DIR}/markers_zh.json   (editable: Chinese marker words)"
echo "  ${INSTALL_DIR}/markers_en.json   (editable: English marker words)"
echo
echo "Run it:"
echo "  ${INSTALL_DIR}/${BINARY} serve"
echo
echo "Tip: edit the markers_*.json files to customize which words produce"
echo "     facts; no recompile needed. Set MNEMOSYNE_HOME to the install"
echo "     directory to point the binary at them explicitly."
