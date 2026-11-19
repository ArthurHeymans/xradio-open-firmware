---
title: Reading the hardware reference
section: Hardware reference
---
# Reading the hardware reference

This reference describes the chip from the firmware CPU’s point of view, not the host SDIO register space.

**Rust layout** tables come from the compiled firmware’s source declarations. `ReadWrite`, `ReadOnly` and `WriteOnly` describe Rust accessors; they do not establish which operations silicon permits. Padding is omitted. Relative layouts such as a descriptor have no single absolute address.

**Observed** means a recorded read, write, value, or experiment. **Inferred** names a proposed role based on those observations. **Hypothesis** means the interpretation still needs a distinguishing test. In particular, a successful association does not validate every bit in the register image.

See [evidence and sources](sources.html) or [open questions](unknowns.html).
