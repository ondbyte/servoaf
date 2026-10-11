#!/usr/bin/env bash
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
#
# Prints servoaf's changes to the vendored servoshell as a unified diff
# against upstream Servo (see crates/servoaf-shell/VENDORED.md).
#
#   scripts/servoshell-diff.sh [servo-git-ref]     # default: v0.7.0
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REF="${1:-v0.7.0}"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

git clone -q --depth 1 --branch "${REF}" --filter=blob:none --sparse \
  https://github.com/servo/servo.git "${WORK}/servo" >&2
git -C "${WORK}/servo" sparse-checkout set ports/servoshell >&2

cd "${WORK}"
mkdir -p a b
cp -r servo/ports/servoshell/. a/
cp -r "${ROOT}/crates/servoaf-shell/." b/
diff -ruN a b || true
