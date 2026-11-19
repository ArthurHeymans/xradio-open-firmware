---
title: Open hardware questions
section: Hardware reference
---
# Open hardware questions

| Item | Status | Test that would settle it |
| --- | --- | --- |
| AES direction-to-IRQ mapping (18 vs 20) | HYPOTHESIS both registered, direction unassigned | Single TX-only and RX-only transfers with one line masked |
| IRQ 4 and 21 handlers | HYPOTHESIS role unknown, left disabled | Trace the vendor callbacks' first register access |
| IRQ 26 role | INFERRED TSF adjustment | Force a TSF sync (beacon TIM) and watch `0x0e34` |
| Routing words 0x0abb_0010..30 | HYPOTHESIS replay is harmless but purposeless | Vary one word and measure interrupt latency per source |
| PHY register semantics | HYPOTHESIS opaque address/value pairs | Per-register ablation against a calibrated throughput baseline |
| Command-word opcode classes not in 9.3 | HYPOTHESIS | Descriptor ISA audit against vendor `txp_desc_emit` |
| Prefetch/fetch state at `0x0e8c` | INFERRED must be written only while quiescent | Cycle it under a running fetch engine and count malformed frames |
| First A-MPDU member loss | INFERRED not controllable from software | External attenuator or an in-channel interferer |
| Absent frames after successful publication (14) | Open blocker | Compare descriptor words and pipe state against vendor immediately before GO |

**Erratum 1: do not re-write the TX ring while the MAC is live.** Writing `ring+0x14` (GO) and cycling `0x0e8c` during operation leaves the final values correct and corrupts fetch behaviour. The vendor programs these exactly once, before MAC core enable.

**Erratum 2: TSF control is edge-sensitive in practice.** Write `0x0e34 = 0` and then `3`. A single write of 3 was the first deviation found in the vendor comparison and remains a suspected cause of rare TSF/event anomalies.

**Erratum 3: the scan filter value excludes compressed BlockAck frames.** The synthetic-scan mode word uses `0x0204 = 0x0019_8000`, which drops received compressed BlockAcks from packet DMA even when the AP sends them and TX status completes. A joined station must use `0x0279_fe00`.
