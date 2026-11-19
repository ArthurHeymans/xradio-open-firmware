---
title: Stack usage check
description: How cargo xtask check bounds the XR819 firmware's stack use from the linked ELF.
---
The firmware runs on 8 KiB of mode stacks in DTCM. `cargo xtask check` cross-builds the release runtime and computes the deepest stack use of its three entry points from the linked ELF. CI runs it on every push. `cargo xtask image` only builds and packs.

The check is a regression test, not a proof of the maximum stack use on every hardware path.

## Result

| Entry point | Includes | Bytes |
|---|---|---:|
| `kmain` | The main loop | 6,160 |
| `_irq_handler` | The entry assembly's 24-byte register save | 48 |
| `xr819_exception_terminal` | The entry assembly's 56-byte register save | 232 |
{: .compact}

Figures for the release runtime of November 2026; run `cargo xtask check` for the current build. They agree with an independent LLVM-based estimate.

## Method

Rust emits a `.stack_sizes` section: one frame size per function. That is a cost, not a call graph. LLVM's call-graph section does not help either: in a trial with nightly `1d81eb4ad`, `-Cllvm-args=--call-graph-section` emitted 255 function records and none listed direct callees. `cargo-call-stack` analyzes Cortex-M machine code, not this ARMv5TE target.

The checker instead reads the ELF with `object` and decodes direct calls with `unarm`:

- It follows the `$a`, `$t` and `$d` mapping symbols, so literal pools are not decoded as instructions.
- It sums frames along direct call paths. A direct tail branch to another function conservatively keeps the caller's frame.
- A direct call cycle, a call through a register, an unresolved target or a missing frame size fails the check.
- Two assembly division helpers have no LLVM frame record. The checker recognizes their audited bodies and accounts for their register saves and local allocation.

## Limits

The checker does not simulate execution. It assumes compiler-generated branches inside a function stay in the same frame, and it does not model computed writes to the program counter or stack pointer. Changes to the entry assembly, stack switching or indirect dispatch need review by hand.
