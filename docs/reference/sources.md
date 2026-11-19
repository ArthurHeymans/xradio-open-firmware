---
title: Sources
description: Where the XR819 reference's facts come from, in this repository and outside it.
---
Paths are relative to the repository root. The Rust sources are the primary record: register layouts, bit fields and doc comments in them generate the tables in this manual.

## In this repository

| Source | Path | Provides |
|---|---|---|
| Platform module | `src/platform.rs` | System, clock, timer and platform register blocks; start-up sequences |
| Interrupts | `src/interrupts.rs` | Interrupt controller layout and sources; the IRQ handler |
| Host interface | `src/hif.rs` | Descriptor format, ring depth, status bits, reclaim order |
| Receive path | `src/radio.rs` | FIFO slot header and trailer, resynchronization |
| Main loop | `src/firmware.rs`, `src/activity.rs` | Pass order, sleep timeout, linger |
| Power save | `src/power_save.rs`, `src/phy/mod.rs` | Station power save, receiver doze |
| A-MPDU | `src/tx/aggregate.rs`, `src/tx/ampdu.rs` | Aggregate command streams, BlockAck resolution |
| MAC | `src/mac.rs` | Mode words, response slots, slot timings, descriptor templates |
| TX pipes | `src/tx/pipe.rs`, `src/tx/event.rs` | Ring layout, `GO`, event and scheduler words |
| AES/CCMP | `src/crypto/engine.rs`, `src/crypto/mod.rs` | Command order, AAD masks, the initialization stream |
| Packet RAM | `src/packet_ram.rs`, `link-main-low.x` | Object layout, address encoders |
| Loader | `src/loader.rs`, `src/download.rs` | The 41 register writes, the PHY tables, the download protocol |
| Image packer | `xtask/src/image.rs` | Container records built from the ELF |
| Vendor data | `src/init_data.rs` | [Vendor initialization data](../init-data.html) |

## Outside this repository

The reverse engineering used an annotated decompilation of the vendor runtime and two notebooks, one on the start-up flow and one on the AES engine. They are not published. Function names taken from them, such as `mac_irq_handler`, are annotations, not vendor symbols.

The vendor image, its container boundaries and the 41 register writes were verified byte for byte against the vendor `fw_xr819.bin` of 130,628 bytes, SHA-256 `3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca`.
