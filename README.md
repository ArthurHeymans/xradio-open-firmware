# XRadio open firmware

This is a Rust replacement for the bootloader and Wi-Fi firmware on the XRadio XR819. It runs on the Orange Pi Zero’s XR819 and speaks the `cw1200` WSM protocol over SDIO. XR829 is planned but has no implementation yet.

## Build for XR819

You'll need Rust nightly with `rust-src`, Python 3, and the LLVM command-line tools. Bring the vendor `fw_xr819.bin` matching the [documented checksum](chips/xr819/data/README.md). The build extracts five initialization files from it.

```sh
rustup component add rust-src --toolchain nightly
cd chips/xr819
python3 tools/extract-init-data.py /path/to/vendor/fw_xr819.bin
tools/check.sh
tools/build-sectioned-bootloader.sh /tmp/boot_xr819.bin
tools/build-ota-image.sh /tmp/fw_xr819.bin
```

Keep your board's `sdd_xr819.bin`. Neither script produces it. The [XR819 notes](chips/xr819/README.md) cover driver requirements, limitations, and the register map.
