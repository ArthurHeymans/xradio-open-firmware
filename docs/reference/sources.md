---
title: Evidence and sources
section: Hardware reference
---
# Evidence and sources

Source paths below are relative to the repository root. The annotated decompilation and two notebooks were in the original development tree and are not shipped here. Their function names are annotations, not vendor symbols.

| Source | Path | Used for |
| --- | --- | --- |
| Vendor container layout | [Section stream](../vendor-container-layout.html) | Section stream, 41 MMIO pairs, four copies |
| Annotated decompilation | xr819-annotated-re-code.md | Function names, argument provenance, ordering claims |
| Startup flow notebook | xr819-hif-startup-flow.md | Top-level order, `0x9ac`/`0xf6` callees, activation, routing words |
| AES interface note | xr819-aes-engine.md | Command words, AAD streams, transfer classes |
| Interrupt and sleep note | docs/interrupts-and-sleep.md | Measured wake behaviour, source survey, sleep-loss table |
| Packet-RAM contract | docs/packet-ram-address-contract.md | Address classes, ownership rules |
| Container/layout note | docs/vendor-container-layout.md | Section table, hardware validation of sectioned download |
| Rust platform module | src/platform.rs | Register blocks, byte-level init sequences, ordering comments |
| Rust MAC module | src/mac.rs | Mode words, response slots, slot timings, descriptor templates |
| Rust HIF module | src/hif.rs | Descriptor format, ring depth, status bits, reclaim ordering |
| Rust radio module | src/radio.rs | FIFO slot header and trailer layout, resynchronization |
| Rust TX pipes | src/tx/pipe.rs, src/tx/event.rs | Ring layout, GO, event scheduler word |
| CCMP setup and frame masking | [engine.rs](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/crypto/engine.rs), [mod.rs](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/crypto/mod.rs) | Separate ports for initialization and key/context input; command order and AAD masks |
| Rust loader | src/loader.rs | The 41 MMIO pairs verbatim |

No web sources were used. The vendor image hash, container boundaries and the 41 register pairs were verified byte-for-byte against the matching container `fw_xr819.bin` (size 0x1fe44, SHA256 3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca).
