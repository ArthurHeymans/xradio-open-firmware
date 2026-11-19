---
title: Interrupt controller
section: Hardware reference
---
# Interrupt controller

The controller is a plain 32-source level/latched aggregator with one FIQ output. The vendor firmware enables individual sources as their owners come up, unmasks FIQ for the MAC event line only, and keeps CPU IRQ masked during startup.

## Register map
{:#s3-1}

{% include registers/InterruptController.md %}

- **CONTROL**: Controller enable. Vendor writes `1` at reset.

- **ACKNOWLEDGE**: Write one bits to clear the corresponding pending source. Writing the value read from `+0x20` is the normal acknowledge.

- **VECTOR_BASE**: The firmware writes `(BASE as u32) << 9`. This is an encoded vector-base word, not a CPU address to dereference.

- **ENABLE**: Bit *n* lets source *n* reach the CPU. The IRQ handler clears the bits of every pending source before acknowledging (mask-on-entry, re-arm by the owner).

- **CLEAR**: Written all-ones during reset, together with an all-ones acknowledge.

- **CONFIGURATION**: Vendor reset value `0x1600_a037`. The top byte selects the FIQ source: `0x16` (22 decimal) names the MAC event line.

- **PENDING**: Pending sources *among the enabled ones*. A disabled source never appears here, which makes this register useless for finding a masked cause.

- **RAW_STATE** (0x24, trace only): Raw source state before the enable mask. Required to see sources 9, 24 and 28, which stay asserted.

## Source assignment
{:#s3-2}

| Source | Owner | Behaviour and handling | Conf. |
| --- | --- | --- | --- |
| 0 | Debug UART transmit | Level, pending whenever nothing is being sent. Any enabled wait-for-interrupt ends immediately. The vendor enables it only while printing; images that print nothing must leave it disabled. | Observed |
| 2 | Debug UART receive | Vendor handler at `0xfff019e0`. Not enabled by the open firmware. | Observed |
| 4 | Unassigned | The vendor registers a handler for it during `0x00000bb0`; its role was not recovered, so the source stays disabled. | Observed |
| 6 | Timer 1 | One-shot expiry. Acknowledged by `platform::wait_for_event` after every sleep, and by the owner when a timer was armed for a deadline. | Observed |
| 13 | HIF | Level. Cause bits live in HIF `+0x24` and are cleared by writing them to `+0x28`; the source is re-armed only after all causes are acknowledged. | Observed |
| 18 | AES engine | Registered during post-activation interrupt setup. Vendor handler `enc_xfer_complete` (`0xea08`). | Observed |
| 20 | AES engine | Second AES line; both are registered together. Direction-to-line mapping is not yet established. | Observed |
| 21 | Unassigned | Vendor registers a handler in `0x00000b88`; purpose unknown. | Observed |
| 26 | Packet DMA / TSF | Vendor IRQ 26 handler at `0x00007e3c` reads MAC `+0x0e34` and clears `0x1f00_0003`. Never observed pending in the open firmware, which never adjusts the TSF. | Inferred |
| 27 | TX event error | Vendor calls `event_send_error_0x34`; disabled until that path is translated. | Observed |
| 28 | MAC event FIFO | Raw bit 28 is set while the MAC event FIFO holds events. Acknowledging it stops RX within seconds, so it is left alone; the FIFO is drained from the main loop. | Inferred |
| FIQ 0x16 | MAC event line | Asserted whenever the MAC has events. It ends a wait-for-interrupt even though it never appears in `+0x20` or `+0x24`; enabling source `0x16` as an IRQ changes nothing and the vendor does not do it. | Observed |

## Interrupt routing words (0x0abb_0010 to 0x0abb_0030)
{:#s3-3}

The vendor writes nine words immediately after subsystem initialization and before enabling interrupts. Values are exact from the instruction stream at `0x000164fa`.

| Address | Value | Note |
| --- | --- | --- |
| 0x0abb_0010 | 0x000 | Replaying the whole group changed nothing measurable on hardware, so the semantics are unproven. |
| 0x0abb_0014 | 0x3fd | Same caveat. |
| 0x0abb_0018 | 0x3fa |  |
| 0x0abb_001c | 0x3ff |  |
| 0x0abb_0020 | 0x01b |  |
| 0x0abb_0024 | 0x05b |  |
| 0x0abb_0028 | 0x0b6 |  |
| 0x0abb_002c | 0x10a |  |
| 0x0abb_0030 | 0x12c |  |

Two more words in the same window come from the container's loader table rather than from startup code: `0x0abb_0008 = 0x0000_026d` and `0x0abb_0078 = 0x0048_0035`.

## Routing block layout

The Rust routing declaration describes offsets, not proof of the purpose of each routing word.

{% include registers/InterruptRouting.md %}
