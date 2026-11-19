---
title: Open questions
description: What is not yet known about the XR819 hardware, and the test that would settle each question.
---
Each question below names the experiment that would settle it. Contributions of measurements are welcome; see [Editing this manual](../contributing-docs.html).

| Question | Current state | Test that would settle it |
|---|---|---|
| Which AES line, IRQ 18 or 20, signals which direction? | Both are registered; which carries which direction is not known | Single TX-only and RX-only transfers with one line masked |
| What do the IRQ 4 and IRQ 21 handlers do? | Unknown; both left disabled | Trace the vendor callbacks' first register access |
| Is IRQ 26 the TSF adjustment? | <span class="tag inferred">inferred</span> | Force a TSF sync from a beacon and watch `TSF_CONTROL` |
| What do the routing words at `0x0abb_0010`–`0x0abb_0030` do? | Replaying them changed nothing measurable; their meaning is unknown | Vary one word at a time and measure per-source interrupt latency |
| What do the PHY register writes mean? | Opaque address/value pairs | Per-register ablation against a calibrated throughput baseline |
| Which command-descriptor opcodes exist beyond the [known ones](rings.html#opcodes)? | The table lists only the opcodes seen in the firmware's descriptors | Audit against the vendor descriptor emitter `txp_desc_emit` |
| What does [`COMMAND_FETCH`](mac.html#mac-command-fetch) control, and does a full 0 → `0xbf` cycle disturb a running engine? | Command fetch, or for bits 5:2 reception <span class="tag hypothesis">hypothesis</span> | Cycle it under a running fetch engine and count malformed frames |
| Can the first A-MPDU member's loss be controlled? | Not from software <span class="tag inferred">inferred</span> | An external attenuator or an in-channel interferer |
| Who clears HIF `STARTUP_STATUS` bit 10? | Unknown | Trace the host's SDIO register writes during probe |
| Why does sleeping between frames lose frames with the "minimal" host driver only? | Unknown; avoided by the current sleep policy | Compare the two drivers' SDIO traffic around a lost frame |
| Why does the `START_REQ` confirmation fail after a rebind? | The kernel logged a failed confirmation for `START_REQ` (`0x0017`) in the [rebind retest](../orange-pi-zero-smoke.html#2026-11-19-rebind-and-wpa2-retest), although the access point then worked | Repeat AP start after a rebind with WSM tracing and compare the confirmation status with a cold boot |
| What are the 430 AES initialization bytes? | Unknown format; needed or not is unknown | Run CCMP without streaming them |
