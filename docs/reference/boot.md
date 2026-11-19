---
title: Image format and boot
description: How the XR819 bootloader is loaded, the download protocol, the XR01 container format and the PHY tables applied before the runtime starts.
---
Booting takes two images. The host first loads a small bootloader, `boot_xr819.bin`; the bootloader then receives the runtime, `fw_xr819.bin`, unpacks it into the chip's memories, initializes the PHY and jumps to it.

## Bootloader

`boot_xr819.bin` is a flat binary loaded at `0x0800_0000`, its entry point. When it starts it:

1. Masks IRQ and FIQ, writes the vendor's CP15 TCM-region and control words, and copies itself to `0x0901_0000` in packet SRAM. The TCM-region writes are in the ARM926 form; they are harmless on the ARM966E-S, whose TCMs are fixed.
2. Sets its stack to the top of usable DTCM, `0x0400_c000`.
3. Runs the download protocol below and unpacks the runtime.
4. Applies the [PHY initialization](#phy-initialization).
5. Waits briefly and jumps to the runtime's entry point.

## Download protocol

The bootloader and the host share a control block at `0x0900_ff80` and a 32 KiB FIFO at `0x0900_4000`, both in packet SRAM.

{% include registers/DownloadControl.md %}

1. The bootloader clears `FLAGS`, `PUT` and `GET`, sets `STATUS` to `0xffff_ffff` (pending) and writes `0x1234_5678` ("I am here") to `IMAGE_SIZE`.
2. The host writes the image size in bytes to `IMAGE_SIZE`. The bootloader waits for any value other than its own `0x1234_5678` and the host's `0x8765_4321` ("are you here") probe.
3. The host copies the image into the FIFO and advances `PUT`. The bootloader reads 32-bit words and advances `GET`.
4. The bootloader writes the final `STATUS`.

`PUT` and `GET` are cumulative byte counts from the start of the image; they do not wrap. Only the FIFO position wraps: byte *n* of the image is at `0x0900_4000 + (n mod 0x8000)`. The host must not let `PUT` run more than `0x8000` bytes ahead of `GET`.

| `STATUS` | Meaning |
|---|---|
| `0xffff_ffff` | Loading. Also what remains if a record runs past the announced size: the bootloader stops without a final status. |
| 0 | The container was accepted and unpacked. The bootloader then applies the PHY initialization and jumps; success does not mean the runtime is up. The runtime's [startup indication](wsm.html#startup-indication) says that. |
| 5 | Bad size: not a multiple of 4, or outside 16 bytes to 1 MiB |
| 6 | Bad content: magic word, record type, destination or entry point |
{: .compact}

## Container format

The runtime image is an `XR01` container: a sequence of records that the bootloader applies in order. All fields are little-endian 32-bit words; lengths are in bytes and must be multiples of 4.

```text
magic   "XR01"
copy    0, destination, length, data[length]
fill    1, destination, value, length
MMIO    2, byte_length, (address, value)[byte_length / 8]
entry   4, entry_address, trailer
```

No record of type 3 is known.

The open bootloader accepts only copy, fill and entry records. A fill's value must be 0, and every destination must be 4-byte aligned and lie entirely inside one of these windows; anything else sets `STATUS` to 6 before any jump:

| Window | Range |
|---|---|
| ITCM | `0x0000_0000`–`0x0001_ffff` |
| DTCM, below the stacks | `0x0400_0000`–`0x0400_9fff` |
| High SRAM | `0xfff0_0000`–`0xfff1_3fff` |
{: .compact}

The entry record must end the stream exactly, and its entry point, ignoring bit 0, must lie in ITCM or high SRAM. The bootloader jumps to the address as given, so bit 0 set enters the runtime in Thumb state. Its trailer word is ignored: the vendor's trailer is not a CRC32 of the stream or of the payloads, and remains opaque.

### Building a container

`cargo xtask image` turns the runtime ELF's loadable segments into records: a copy for each segment's bytes, with zero runs of 64 bytes or more and the zero-initialized tail emitted as fill records, then one entry record. Nothing may load into packet SRAM, and DTCM loads must stay below the stacks.

<div class="note" markdown="1">
Do not convert the ELF with `objcopy -O binary`. ITCM, DTCM and high SRAM are far apart, and a flat image would fill the gaps between them.
</div>

## Vendor container

For reference, the vendor's `fw_xr819.bin` (130,628 bytes, SHA-256 `3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca`) holds these records:

| # | Kind | File offset | Destination | Length | Contents |
|---:|---|---:|---|---:|---|
| 0 | copy | `0x00004` | `0xfff0_0000` | `0x1a7c` | High bootstrap and support routines, ARM |
| 1 | fill | `0x01a8c` | `0xfff0_1a7c` | `0x1d24` | Zero |
| 2 | fill | `0x01a9c` | `0xfff0_37a0` | `0x1_0860` | Zero, up to `0xfff1_4000` |
| 3 | copy | `0x01aac` | `0x0000_0000` | `0x1_b3d0` | Main image, Thumb, ITCM |
| 4 | copy | `0x1ce88` | `0x0400_0000` | `0x10f8` | Initialized DTCM |
| 5 | copy | `0x1df8c` | `0x0400_10f8` | `0x0f80` | Initialized DTCM |
| 6 | fill | `0x1ef18` | `0x0400_2078` | `0x7bcc` | Zero, up to `0x0400_9c44` |
| 7 | MMIO | `0x1ef28` | — | `0x148` | 41 register writes |
| 8 | copy | `0x1f078` | `0x0ab8_1000` | `0x400` | PHY table |
| 9 | copy | `0x1f484` | `0x0ab8_8400` | `0x330` | PHY table |
| 10 | copy | `0x1f7c0` | `0x0ab8_8800` | `0x330` | PHY table |
| 11 | copy | `0x1fafc` | `0x0ab8_8c00` | `0x330` | PHY table |
| 12 | entry | `0x1fe38` | `0xfff0_0000` | — | Trailer `0x3994_defa` |
{: .compact}

## PHY initialization

Records 7 to 11 initialize the PHY. They belong to image loading, not to the runtime, so the open bootloader applies them itself after unpacking the runtime and before jumping to it. [PHY and RF](phy.html#loader-tables) lists them.
