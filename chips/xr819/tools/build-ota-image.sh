#!/usr/bin/env bash
# Build and pack the production firmware. Run tools/check.sh for the full checks.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 OUTPUT.bin" >&2
  exit 2
fi
OUT=$(realpath "$1")

cd "$(dirname "$0")/.."

cargo +nightly build --release --bin hif-startup --target thumbv5te-none-eabi \
  -Z build-std=core
ELF=${CARGO_TARGET_DIR:-../../target}/thumbv5te-none-eabi/release/hif-startup
python3 tools/check-stack.py "$ELF"
python3 tools/pack-sectioned-elf.py "$ELF" "$OUT"
