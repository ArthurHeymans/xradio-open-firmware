# Vendor-derived initialization data

These five local build inputs are copied from `fw_xr819.bin`. The filenames describe where or how the bytes are used, not a decoded instruction set or a license. The files are not tracked in this source tree; `include_bytes!` embeds them in the built images.

From `chips/xr819/`, prepare them from a local copy of the matching XR01 container:

```sh
python3 tools/extract-init-data.py /path/to/fw_xr819.bin
```

The extractor checks the container SHA-256 (`3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca`, the image in the local `xr819-fw.tar.gz`) before using these file offsets. It prints a checksum per output and refuses to overwrite different contents.

| Build input | Container offset | Bytes | SHA-256 |
|---|---:|---:|---|
| `engine-mode1-init.bin` | `0x1d6c4` | 430 | `211ad6ec637a780d770998fbe76bcefa1b127506ef8e0f9f09539da32d881b4c` |
| `phy-init-ab81000.bin` | `0x1f084` | 1024 | `2ec66edfbaf0edc13e8e953aa25d787905d28dfd9783b3bf6bb85cca2fbdbf51` |
| `phy-init-ab88400.bin` | `0x1f490` | 816 | `06eead5e4f54abd15d4086b9a9928fa5dd14bec4e82c45a44b3120fad250eafa` |
| `phy-init-ab88800.bin` | `0x1f7cc` | 816 | `fbcfd3e5ef2ac22cd067a368c9c167195329503f962431fcb7f5d5c877a033b3` |
| `phy-init-ab88c00.bin` | `0x1fb08` | 816 | `46249a54907cbfbcce5210ddc9d9d31cdb7b1ff58d5551ac63a1cc7ede558d2f` |

The downloader writes the four PHY tables as little-endian words at the addresses in their names, after 41 ordered MMIO writes. They are distinct from the board's SDD calibration file. See [container layout](../docs/vendor-container-layout.md).

The engine payload (formerly `aes-mode1.bin`) comes from DTCM `[0x04000830, 0x040009de)`. The mode-selection routine at `0x15fe0` in the annotated main image streams the first byte with bit 31 set, then the remaining bytes, to `0x09c50008`, followed by command `0x9000`. Neither that upload nor the CCMP known-answer tests establish whether the payload contains instructions, a table, or another encoding. The original extraction record names a second container, SHA-256 `fb81436ad7cc0876614a2a9c2a54c5a93a75315aee164e3a3afe3db80842a9e1`, offset `0x1cdf8 + 0x830`, with the same payload hash. For existing checkouts the old PHY filenames were `vendor-ab*.bin`; only their names changed.
