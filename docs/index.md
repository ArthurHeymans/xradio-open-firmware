---
layout: default
title: XR819 documentation
---
# XR819 documentation

[Using the firmware with Linux](linux-driver.html) covers the `cw1200` driver, the two firmware images, and the calibration file supplied with the board.

## Hardware reference

- [Register map](xr819-hardware-reference.html#map): addresses, register fields, and the order in which the firmware programs them.
- [Packet RAM](packet-ram-address-contract.html): how CPU pointers differ from the addresses the MAC and DMA engines use.
- [Interrupts and sleep](interrupts-and-sleep.html): which events wake the radio and how the main loop waits for them.

## Firmware images

- [Image format](vendor-container-layout.html): the XR01 copy, fill and entry records.
- [Initialization data](init-data.html): the four PHY tables and the engine initialization payload, with their source offsets and hashes.

## Test notes

- [Orange Pi Zero](orange-pi-zero-smoke.html): tested driver and firmware revisions, station and AP traffic, and SDIO rebind results.
- [Stack usage](stack-check.html): what the static check measures and where its estimates stop.
