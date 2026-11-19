# XRadio open firmware

Firmware for the XRadio XR819 Wi-Fi chip, written in Rust.

```sh
cargo xtask image
```

The bootloader, firmware and radio configuration file (`sdd_xr819.bin`) are
written to `target/images/`. Install them in `/lib/firmware/xr819/`.

[Documentation](https://arthurheymans.github.io/xradio-open-firmware/)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. This covers the source, the built
images and `firmware/sdd_xr819.bin`.
