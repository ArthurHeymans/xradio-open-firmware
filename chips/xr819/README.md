# XR819

The bootloader starts in SRAM, copies the PHY initialization tables, and loads the firmware into ITCM and DTCM. Linux loads `xr819/boot_xr819.bin`, `xr819/fw_xr819.bin`, and the board’s original `xr819/sdd_xr819.bin` through `cw1200`. Stock Linux without XR819 probe and bootloader support is not enough.

The firmware supports WPA2-CCMP station and access point operation on 2.4 GHz, one stream, 20 MHz. It does not implement WEP, TKIP, IBSS or 5 GHz. A successful association is not a throughput or endurance qualification.

- [Build instructions](../../README.md#build-for-xr819)
- [Register reference](../../docs/xr819-hardware-reference.html) (open the HTML locally)
- [Vendor inputs and checksums](data/README.md)

The firmware began as a translation of decompiled vendor firmware. The crate's license declaration is not a license grant for extracted vendor material. This repository contains source and an extractor, not installable images.
