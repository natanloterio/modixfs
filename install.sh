#!/usr/bin/env bash
set -euo pipefail

REPO="natanloterio/modixfs"
BIN="modixfs"
INSTALL_DIR="/usr/local/bin"

# ── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BOLD='\033[1m'; RESET='\033[0m'
info()  { printf "  ${BOLD}%s${RESET}\n" "$*"; }
ok()    { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
warn()  { printf "  ${YELLOW}!${RESET} %s\n" "$*"; }
die()   { printf "\n  ${RED}error:${RESET} %s\n\n" "$*" >&2; exit 1; }

# ── detect OS ────────────────────────────────────────────────────────────────
case "$(uname -s)" in
  Linux)  OS="linux" ;;
  Darwin) OS="macos" ;;
  *)      die "Unsupported OS: $(uname -s)" ;;
esac

# ── detect arch ──────────────────────────────────────────────────────────────
case "$(uname -m)" in
  x86_64)          ARCH="x86_64" ;;
  aarch64|arm64)   ARCH="aarch64" ;;
  *)               die "Unsupported architecture: $(uname -m)" ;;
esac

ASSET="${BIN}-${OS}-${ARCH}"

# ── check dependencies ───────────────────────────────────────────────────────
check_fuse() {
  if [ "$OS" = "linux" ]; then
    if ! pkg-config --exists fuse3 2>/dev/null && ! dpkg -l fuse3 2>/dev/null | grep -q '^ii'; then
      warn "fuse3 not detected. Install it with:"
      warn "  sudo apt-get install fuse3    # Debian/Ubuntu"
      warn "  sudo dnf install fuse3        # Fedora/RHEL"
    else
      ok "fuse3 found"
    fi
  elif [ "$OS" = "macos" ]; then
    if ! [ -d "/Library/Filesystems/macfuse.fs" ] && ! [ -d "/usr/local/lib/pkgconfig" ]; then
      warn "macFUSE not detected. Install it from: https://osxfuse.github.io"
      warn "Then re-run this script."
    else
      ok "macFUSE found"
    fi
  fi
}

# ── resolve latest version ───────────────────────────────────────────────────
resolve_version() {
  local url="https://api.github.com/repos/${REPO}/releases/latest"
  if command -v curl &>/dev/null; then
    curl -fsSL "$url" | grep '"tag_name"' | sed 's/.*"tag_name": *"\(.*\)".*/\1/'
  elif command -v wget &>/dev/null; then
    wget -qO- "$url" | grep '"tag_name"' | sed 's/.*"tag_name": *"\(.*\)".*/\1/'
  else
    die "curl or wget is required"
  fi
}

# ── download ─────────────────────────────────────────────────────────────────
download() {
  local url="$1" dest="$2"
  if command -v curl &>/dev/null; then
    curl -fsSL --progress-bar "$url" -o "$dest"
  else
    wget -q --show-progress "$url" -O "$dest"
  fi
}

# ── main ─────────────────────────────────────────────────────────────────────
printf "\n  ${BOLD}ModixFS installer${RESET}\n\n"

info "Detecting platform: ${OS}/${ARCH}"
check_fuse

info "Resolving latest release..."
VERSION="$(resolve_version)"
[ -z "$VERSION" ] && die "Could not determine latest release. Check your internet connection."
ok "Latest release: ${VERSION}"

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

info "Downloading ${ASSET}..."
download "$DOWNLOAD_URL" "$TMP" || die "Download failed: ${DOWNLOAD_URL}"
chmod +x "$TMP"

info "Installing to ${INSTALL_DIR}/${BIN}..."
if [ -w "$INSTALL_DIR" ]; then
  mv "$TMP" "${INSTALL_DIR}/${BIN}"
else
  sudo mv "$TMP" "${INSTALL_DIR}/${BIN}"
fi

ok "Installed: $(command -v ${BIN})"
printf "\n  ${GREEN}${BOLD}Done.${RESET} Run ${BOLD}modixfs init${RESET} to get started.\n\n"
