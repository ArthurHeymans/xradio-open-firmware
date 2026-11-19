---
layout: default
title: Using the firmware with Linux
---
# Using the firmware with Linux

The XR819 connects over SDIO. The Linux driver is `cw1200`; it needs the XR819 probe and firmware-download changes in the [driver series](https://github.com/ArthurHeymans/linux/commits/cw1200-xr819-review/). Enable `CONFIG_CW1200` and `CONFIG_CW1200_WLAN_SDIO` when building the kernel. [Board test notes](orange-pi-zero-smoke.html) identify the revisions actually run on the Orange Pi Zero.

From a firmware checkout with Rust installed through rustup, run `cargo xtask image`. The driver requests these three files under the Linux firmware directory (usually `/lib/firmware/`):

| Driver request | Where it comes from |
|---|---|
| `xr819/boot_xr819.bin` | Bootloader built in `target/images/` |
| `xr819/fw_xr819.bin` | Runtime built in `target/images/` |
| `xr819/sdd_xr819.bin` | Calibration from the board’s original installation |

Back up the installed files before replacing the bootloader and runtime. Leave `sdd_xr819.bin` in place: the two built images do not replace board calibration. On a successful probe the kernel log identifies the runtime as `XR819 open Rust native`.
