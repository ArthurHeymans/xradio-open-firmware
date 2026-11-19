# XRadio open firmware

Rust firmware and bootloader for the Orange Pi Zero’s XR819. It speaks the `cw1200` WSM protocol over SDIO and supports WPA2-CCMP station and access point operation at 2.4 GHz. `xr819` is the default Cargo feature. XR829 is planned, not implemented.

```sh
cargo xtask image
```

This writes `boot_xr819.bin` and `fw_xr819.bin` to `target/images/`. No vendor image is needed to build them. Keep your board’s original `sdd_xr819.bin` for calibration.

Before submitting a change, run `cargo test --workspace` and `cargo xtask check`. The latter checks the linked firmware’s [stack usage](docs/stack-check.md); neither command needs LLVM utilities on `PATH`.

The [Firmware checks workflow](https://github.com/ArthurHeymans/xradio-open-firmware/actions/workflows/check.yml) builds `main` nightly at 03:23 UTC. Successful main-branch runs attach both images and `SHA256SUMS` for 14 days. These are CI builds, not board-tested releases.

[Driver and board tests](docs/orange-pi-zero-smoke.md) · [Register reference](docs/xr819-hardware-reference.html) · [Initialization data](docs/init-data.md)
