#!/usr/bin/env bash
# Regenerates src-tauri/icons from icon.svg through plate (github.com/lockyc/plate): the .icns and
# the PNGs Tauri bundles. Run via `just icon`.
set -euo pipefail
cd "$(dirname "$0")/../src-tauri/icons"
plate icon --icns icon.icns \
  --png 512:icon.png --png 256:128x128@2x.png --png 128:128x128.png --png 32:32x32.png \
  icon.svg
