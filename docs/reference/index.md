---
title: Memory map
description: The XR819 firmware CPU's address space, its memories and its register blocks.
---
The ARM966E-S core sees a sparse 32-bit address space. Code and data live in two tightly coupled memories, packet memory is a separate SRAM shared with the hardware engines, and the register blocks sit between the MAC at `0x09c0_0000` and the platform control area at `0x0ac8_0000`, whose last known word is `0x0ac8_00e8`, beyond the typed layout listed below.

<figure>
{% include datapath.svg %}
<figcaption>Frames cross packet SRAM in both directions. The CPU programs the blocks and builds descriptors; the frames themselves move by DMA.</figcaption>
</figure>

## Memories

| Range | Memory | Contents |
|---|---|---|
| `0x0000_0000`–`0x0001_ffff` | ITCM, 128 KiB | Code and read-only data |
| `0x0400_0000`–`0x0400_9fff` | DTCM, 40 KiB | `.data`, `.bss` and the exception record |
| `0x0400_a000`–`0x0400_bfff` | DTCM, 8 KiB | Mode stacks |
| `0x0900_7000`–`0x0901_662f` | Packet SRAM | Runtime pool shared with the hardware ([objects](#packet-sram-objects)) |
| `0x0940_0000`–`0x0940_7fff` | Packet SRAM | RX FIFO |
| `0xfff0_0000`–`0xfff1_3fff` | High SRAM | Vendor bootstrap and support routines |
{: .nowrap-first .nowrap-second}

Before the runtime starts, the [bootloader](boot.html#bootloader) uses parts of packet SRAM that the runtime later takes over:

| Range | Use |
|---|---|
| `0x0800_0000` | Load address and entry of `boot_xr819.bin`; it copies itself away at once |
| `0x0900_4000`–`0x0900_bfff` | Download FIFO |
| `0x0900_ff80`–`0x0900_ffff` | Download control block |
| `0x0901_0000`–`0x0901_49a7` | Relocated bootloader code, data and BSS, at most |
{: .compact .nowrap-first}

The CPU identifies as `0x41259661` (CP15 ID). Its TCM size register reads `0x001c_0200`, which describes 64 KiB of DTCM, but DTCM above `0x0400_c000` does not read back what was written: 48 KiB are usable.

## Register blocks

{% include registers/map.md %}

The PHY and RF window also holds the vendor's [interrupt routing words](vendor.html#interrupt-routing-words) at `0x0abb_0010`. The debug UART at `0x0a90_0000` has no layout here; see [interrupt sources](interrupts.html#interrupt-sources-bits). The [image format](boot.html) describes how the loader fills ITCM, DTCM and high SRAM.

## Packet SRAM objects

The firmware and four hardware engines share the runtime pool: the HIF, the MAC's TX pipes and response engine, receive packet DMA and the AES engine. Each object is a separate linker section, placed in this order:

| Address | Object | Count × size | Used by |
|---|---|---|---|
| `0x0900_7000` | Response pointers | 32 × 4 | MAC response engine, through `0x09c0_0c00` |
| `0x0900_7080` | TX command slots | 4 pipes × 4 × `0x54` | MAC TX pipes fetch them |
| `0x0900_75c0` | Rate RAM | 80 × `0x10`, then `0x100` | MAC, through `0x09c0_0800` |
| `0x0900_7bc0` | Duration words | 2 × 2 | Command opcode `0x21` |
| `0x0900_7bc4` | Response commands | 13 × `0x54` | MAC response engine: beacon, ACK, CTS, BlockAck |
| `0x0900_8008` | Interface metadata | 4 | MAC ORs one byte into frame control |
| `0x0900_800c` | HIF input buffers | 30 × `0x660` | Host writes, firmware reads |
| `0x0901_3f4c` | HIF output buffers | 4 × `0x180` | Firmware writes, host reads |
| `0x0901_454c` | Internal TX buffers | 3 × `0x400` | Frames the firmware builds itself, such as probe requests |
| `0x0901_514c` | Software records | 4 × `0x2a0` | A-MPDU transfer records, one per pipe |
| `0x0901_5bcc` | Automatic response list | `0x8c` | MAC; head `0x4e14_0000`, 33 entries, terminator `0xf000_0000` |
| `0x0901_5c58` | Host frame states | 30 × `0x54` | Retained context of each published frame |
| `0x0940_0000` | RX FIFO | `0x8000` | Packet DMA writes, firmware reads |

Addresses are from the current link and follow from the section order in `src/packet_ram.rs`. The rate RAM keeps `0x100` bytes after its last entry because the MAC expects the response objects at that distance. The hardware never sees these CPU addresses; it sees the [encoded forms](addresses.html).
