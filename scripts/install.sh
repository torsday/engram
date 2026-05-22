#!/usr/bin/env sh
# install.sh — download and install the latest engram binary
# Usage: curl -fsSL https://raw.githubusercontent.com/torsday/engram/main/scripts/install.sh | sh

set -eu

REPO="torsday/engram"
INSTALL_DIR="/usr/local/bin"
BINARY="engram"

# --- Detect platform ---
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Darwin)
    case "${ARCH}" in
      arm64)  TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin" ;;
      *)
        echo "Unsupported macOS architecture: ${ARCH}" >&2
        exit 1
        ;;
    esac
    ;;
  Linux)
    case "${ARCH}" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *)
        echo "Unsupported Linux architecture: ${ARCH}. Only x86_64 is supported." >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "Unsupported OS: ${OS}. engram supports macOS and Linux." >&2
    exit 1
    ;;
esac

# --- Fetch latest release version ---
echo "Fetching latest engram release..."
LATEST_URL="https://api.github.com/repos/${REPO}/releases/latest"
VERSION="$(curl -fsSL "${LATEST_URL}" | grep '"tag_name"' | sed 's/.*"tag_name": *"\(.*\)".*/\1/')"

if [ -z "${VERSION}" ]; then
  echo "Could not determine latest release version." >&2
  exit 1
fi

VERSION_NUM="${VERSION#v}"
ARCHIVE="engram-${VERSION_NUM}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

# --- Download ---
echo "Downloading ${ARCHIVE}..."
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

curl -fsSL "${DOWNLOAD_URL}" -o "${TMP_DIR}/${ARCHIVE}"
tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "${TMP_DIR}"

if [ ! -f "${TMP_DIR}/${BINARY}" ]; then
  echo "Binary '${BINARY}' not found in archive." >&2
  exit 1
fi

chmod +x "${TMP_DIR}/${BINARY}"

# --- Install ---
if [ -w "${INSTALL_DIR}" ]; then
  mv "${TMP_DIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
else
  echo "Installing to ${INSTALL_DIR} requires sudo..."
  sudo mv "${TMP_DIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
fi

echo ""
echo "engram ${VERSION_NUM} installed to ${INSTALL_DIR}/${BINARY}"
echo "Run 'engram --version' to verify."
