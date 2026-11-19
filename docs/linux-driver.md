---
title: Build and install
description: Build the XR819 bootloader and runtime images and install them for the Linux cw1200 driver.
---
The firmware ships as two images, a bootloader and a runtime, and a radio configuration file. The Linux `cw1200` driver downloads the images over SDIO when it probes the chip, and passes the configuration file to the runtime.

## Build the images

Install Rust with [rustup](https://rustup.rs/). The repository pins its nightly toolchain in `rust-toolchain.toml`, so the first build downloads it. From the repository root:

```sh
cargo xtask image
```

This writes `boot_xr819.bin`, `fw_xr819.bin` and `sdd_xr819.bin` to `target/images/`. The build is reproducible: a fresh checkout produces byte-identical images. CI also builds them on every push to `main` and keeps them for 14 days, with their SHA-256 sums, as a workflow artifact named `xr819-<commit>`.

## Prepare the kernel

Mainline `cw1200` does not know the XR819 yet. Build a kernel with the XR819 probe and firmware-download changes from the [driver series](https://github.com/ArthurHeymans/linux/commits/cw1200-xr819-review/), and enable:

- `CONFIG_CW1200`
- `CONFIG_CW1200_WLAN_SDIO`

The [board test reports](orange-pi-zero-smoke.html) name the kernel revisions that have run on hardware.

## Install the firmware

The driver requests three files from the `xr819/` subdirectory of the firmware directory, usually `/lib/firmware/`. `cargo xtask image` writes all three to `target/images/`:

| File | What it is |
|---|---|
| `boot_xr819.bin` | The bootloader, built from this repository |
| `fw_xr819.bin` | The firmware, built from this repository |
| `sdd_xr819.bin` | Radio configuration data, copied from [`firmware/sdd_xr819.bin`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/firmware/sdd_xr819.bin); the same file the vendor ships for every XR819 board |

1. Back up the existing `xr819/` directory.
2. Copy the three files into it.

## Check the result

Reload the driver or reboot. On a successful probe the kernel log names the runtime:

```text
XR819 open Rust native
```

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| The driver reports that `xr819/boot_xr819.bin` or `fw_xr819.bin` is missing | The images are not in the firmware directory under the `xr819/` subdirectory. Copy them there. |
| The driver reports that `xr819/sdd_xr819.bin` is missing | The configuration file was not installed. Copy it from `target/images/`. |
| Binding fails with `-EINVAL` after an unbind | The chip still runs the previous firmware and cannot accept a new download. Use a kernel whose XR819 SDIO support resets the card on probe, as [`9989efc3a837`](https://github.com/ArthurHeymans/linux/commit/9989efc3a837) in the driver series does. |
| The log does not show `XR819 open Rust native` | First check that the probe and the firmware download succeeded. If they did, the runtime that started is not this one: check that the copied `fw_xr819.bin` is the one from `target/images/`. |
