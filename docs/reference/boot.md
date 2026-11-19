---
title: Boot and download
section: Hardware reference
---
# Boot and download

Before any firmware code runs, a ROM or SDIO bootloader parses a container and programs the device. The container is a sequence of typed sections; the bootloader applies each one in file order and then jumps to the entry address.

```
magic:  "XR01"
copy:   type = 0, destination, length, data[length]
fill:   type = 1, destination, value, length
MMIO:   type = 2, byte_length, (address, value)[byte_length / 8]
entry:  type = 4, entry_address, trailer
```

### Reference container layout

| # | Kind | Destination | Length | Contents |
| --- | --- | --- | --- | --- |
| 0 | copy | 0xfff0_0000 | 0x001a7c | High ARM bootstrap and support routines |
| 1 | fill | 0xfff0_1a7c | 0x001d24 | Zero, up to `0xfff037a0` |
| 2 | fill | 0xfff0_37a0 | 0x010860 | Zero, ends at `0xfff14000` |
| 3 | copy | 0x0000_0000 | 0x01b3d0 | Main Thumb/ITCM image |
| 4 | copy | 0x0400_0000 | 0x0010f8 | Initialized DTCM, first part |
| 5 | copy | 0x0400_10f8 | 0x000f80 | Initialized DTCM, second part |
| 6 | fill | 0x0400_2078 | 0x007bcc | Zero, ends at `0x04009c44` |
| 7 | MMIO | address/value pairs | 0x000148 | 41 PHY/MAC pairs, [PHY and RF](phy.html#s11-2) |
| 8 | copy | 0x0ab8_1000 | 0x000400 | MAC/PHY table |
| 9 | copy | 0x0ab8_8400 | 0x000330 | MAC/PHY table |
| 10 | copy | 0x0ab8_8800 | 0x000330 | MAC/PHY table |
| 11 | copy | 0x0ab8_8c00 | 0x000330 | MAC/PHY table |
| 12 | entry | 0xfff0_0000 | - | Trailer `0x3994defa`, not a CRC32 of the stream |

For a rebuilt image, do not use `objcopy -O binary`: the holes between ITCM, DTCM and high SRAM are enormous. Convert ELF program headers into compact copy/fill records instead. Accepted write ranges are the observed ITCM, DTCM (`0x0400_0000..0x0400_a000`) and high-support (`0xfff0_0000..0xfff1_4000`) windows; anything else is rejected before the jump.

**Ordering proof point.** The five PHY/MAC load groups are part of image loading, not firmware behaviour. A rebuild that omits them still boots and links, then fails in ways that look like MAC or calibration bugs. Reapplying all five groups before entering the low image keeps scan, probe, configuration and empty-scan completion working.
