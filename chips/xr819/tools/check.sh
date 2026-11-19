#!/usr/bin/env bash
# Checks every commit must pass: host tests, the ARM build, the call-chain stack
# bound, the packer (segment/section rules) and the bootloader build.
# Hardware behaviour is covered separately by tools/hw/.
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET=thumbv5te-none-eabi
ELF="${CARGO_TARGET_DIR:-../../target}/$TARGET/release/hif-startup"
PACKED=$(mktemp)
BOOTSTRAP=$(mktemp)
trap 'rm -f "$PACKED" "$BOOTSTRAP"' EXIT

echo "== host tests =="
cargo +nightly test
python3 tools/test-pack-sectioned-elf.py
python3 tools/hw/test-sequence-flow.py

echo "== arm build, stack bound, pack =="
cargo +nightly build --release --bin hif-startup --target "$TARGET" -Z build-std=core
stack=$(python3 tools/check-stack.py "$ELF")
echo "$stack"
# A path the compiler proves always halts (a register access outside its
# window, say) prunes everything after it, and the stack bound then passes
# on a near-empty image. The main loop must still be reachable.
echo "$stack" | grep -q 'Firmware>::run' \
  || { echo "FAIL: the main loop is unreachable from kmain"; exit 1; }
python3 tools/pack-sectioned-elf.py "$ELF" "$PACKED"

echo "== bootloader =="
./tools/build-sectioned-bootloader.sh "$BOOTSTRAP"

echo "ALL CHECKS PASSED"
