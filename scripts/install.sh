#!/usr/bin/env bash
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
#
# Installs a prebuilt servoaf from an extracted release archive.
#
#   ./install.sh                 # -> ~/.local/bin/servoaf
#   PREFIX=/usr/local sudo -E ./install.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-${HOME}/.local}"
BIN_DIR="${PREFIX}/bin"

log() { printf '\033[1;34m[servoaf]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[servoaf]\033[0m %s\n' "$*"; }

[[ -x "${HERE}/servoaf" ]] || { echo "servoaf binary not found next to install.sh" >&2; exit 1; }

if [[ "$(uname -m)" != x86_64 ]]; then
  warn "this build is for x86_64; this machine is $(uname -m)"
fi

# The build was made on Ubuntu 24.04 and needs glibc 2.39 or newer.
glibc="$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
if [[ -n "${glibc}" ]] && [[ "$(printf '%s\n2.39\n' "${glibc}" | sort -V | head -1)" != 2.39 ]]; then
  warn "glibc ${glibc} is older than 2.39; this build will not run here."
  warn "Use Ubuntu 24.04+, Debian 13+, Fedora 40+, or build from source (make deps build)."
fi

missing="$(ldd "${HERE}/servoaf" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ')"
# Loaded at runtime (not visible to ldd): EGL/GL for rendering, xkbcommon for the keyboard.
libcache="$(ldconfig -p 2>/dev/null || true)"
for lib in libEGL.so.1 libGL.so.1 libxkbcommon.so.0; do
  [[ "${libcache}" == *"${lib}"* ]] || missing+="${lib} "
done
if [[ -n "${missing// /}" ]]; then
  warn "missing runtime libraries: ${missing}"
  warn "Debian/Ubuntu: sudo apt install libegl1 libgl1-mesa-dri libfontconfig1 libfreetype6 libxkbcommon-x11-0"
  warn "Fedora:        sudo dnf install mesa-libEGL mesa-dri-drivers fontconfig freetype libxkbcommon-x11"
fi

mkdir -p "${BIN_DIR}"
install -m 0755 "${HERE}/servoaf" "${BIN_DIR}/servoaf"
log "installed ${BIN_DIR}/servoaf"
case ":${PATH}:" in
  *":${BIN_DIR}:"*) ;;
  *) warn "${BIN_DIR} is not on your PATH; add: export PATH=\"${BIN_DIR}:\$PATH\"" ;;
esac
log "try it:  servoaf https://example.com      (MCP at http://127.0.0.1:9223/mcp)"
log "         servoaf --help"
