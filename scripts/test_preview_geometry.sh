#!/bin/bash
set -euo pipefail
binary=$(mktemp -t myremoteapp-preview-geometry)
trap 'rm -f "$binary"' EXIT
xcrun clang -fobjc-arc -Wall -Wextra -Werror \
  -framework Cocoa -framework QuartzCore \
  "$(dirname "$0")/test_preview_geometry.m" -o "$binary"
"$binary"
