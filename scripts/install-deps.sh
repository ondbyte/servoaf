#!/usr/bin/env bash
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
#
# Installs what building servoaf (and Servo) needs on Linux: a C/C++
# toolchain with clang, a few -dev libraries for windowing, fonts and GL, and
# the Rust toolchain from rust-toolchain.toml.
#
#   scripts/install-deps.sh          # install (uses sudo when not root)
#   scripts/install-deps.sh --check  # only report what is missing
#
# The package lists follow Servo's python/servo/platform/linux_packages,
# without GStreamer (servoaf does not enable media playback).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

APT_PACKAGES=(
  build-essential ca-certificates clang cmake curl git gperf llvm-dev m4
  pkg-config python3 libdbus-1-dev libfreetype6-dev libfontconfig1-dev
  libharfbuzz-dev liblzma-dev libudev-dev libunwind-dev libssl-dev
  libgl1-mesa-dri libgles2-mesa-dev libegl1-mesa-dev libglib2.0-dev
  libvulkan1 mesa-vulkan-drivers libx11-dev libxcb-render0-dev
  libxcb-shape0-dev libxcb-xfixes0-dev libxmu-dev libxkbcommon-dev
  libxkbcommon-x11-0 xorg-dev fonts-dejavu-core fonts-liberation
  fonts-noto-color-emoji xvfb
)
DNF_PACKAGES=(
  gcc gcc-c++ clang clang-libs llvm cmake git gperf m4 pkgconf python3
  libtool dbus-devel freetype-devel fontconfig-devel harfbuzz-devel
  expat-devel libunwind-devel openssl-devel mesa-libGL-devel
  mesa-libEGL-devel glib2-devel libX11-devel libXrandr-devel libXi-devel
  libXcursor-devel libXmu-devel libxkbcommon-devel libxkbcommon-x11
  vulkan-loader mesa-vulkan-drivers dejavu-fonts-all
  xorg-x11-server-Xvfb
)
PACMAN_PACKAGES=(
  base-devel clang cmake git gperf llvm m4 pkgconf python dbus freetype2
  fontconfig harfbuzz libunwind openssl mesa glib2 libx11 libxcb libxmu
  libxkbcommon libxkbcommon-x11 vulkan-icd-loader ttf-dejavu xorg-server-xvfb
)

log() { printf '\033[1;34m[servoaf]\033[0m %s\n' "$*"; }

sudo_cmd() {
  if [[ ${EUID} -eq 0 ]]; then "$@"; else sudo "$@"; fi
}

check() {
  local missing=()
  for tool in cc c++ clang cmake pkg-config python3 git; do
    command -v "${tool}" >/dev/null 2>&1 || missing+=("${tool}")
  done
  for lib in fontconfig freetype2 x11 xkbcommon egl; do
    pkg-config --exists "${lib}" 2>/dev/null || missing+=("lib${lib} (dev)")
  done
  command -v cargo >/dev/null 2>&1 || missing+=("cargo (https://rustup.rs)")
  if ((${#missing[@]})); then
    log "missing: ${missing[*]}"
    return 1
  fi
  log "build dependencies OK"
}

install_system_packages() {
  if command -v apt-get >/dev/null 2>&1; then
    sudo_cmd apt-get update
    # Skip names this release doesn't have instead of failing.
    local available=()
    for package in "${APT_PACKAGES[@]}"; do
      if apt-cache show "${package}" >/dev/null 2>&1; then available+=("${package}"); fi
    done
    sudo_cmd env DEBIAN_FRONTEND=noninteractive apt-get install -y "${available[@]}"
  elif command -v dnf >/dev/null 2>&1; then
    sudo_cmd dnf install -y --skip-unavailable "${DNF_PACKAGES[@]}" ||
      sudo_cmd dnf install -y "${DNF_PACKAGES[@]}"
  elif command -v pacman >/dev/null 2>&1; then
    sudo_cmd pacman -S --needed --noconfirm "${PACMAN_PACKAGES[@]}"
  else
    log "unknown package manager; install the equivalent of: ${APT_PACKAGES[*]}"
    return 1
  fi
}

install_rust() {
  if ! command -v rustup >/dev/null 2>&1; then
    log "installing rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
  fi
  # Installs the channel pinned in rust-toolchain.toml.
  (cd "${ROOT}" && rustup show active-toolchain >/dev/null)
}

case "${1:-}" in
  --check) check ;;
  "")
    install_system_packages
    install_rust
    check
    log "done; now run: make build"
    ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac
