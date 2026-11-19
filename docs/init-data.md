# XR819 initialization constants

[`src/init_data.rs`](../src/init_data.rs) holds these five vendor-derived byte arrays. They are literal constants, not generated at build time. The offsets identify their source in the XR01 `fw_xr819.bin` with SHA-256 `3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca`.

| Constant | File offset | Bytes | SHA-256 (in source byte order) |
|---|---:|---:|---|
| `ENGINE_MODE1_INIT` | `0x1d6c4` | 430 | `211ad6ec637a780d770998fbe76bcefa1b127506ef8e0f9f09539da32d881b4c` |
| `PHY_INIT_AB81000` | `0x1f084` | 1024 | `2ec66edfbaf0edc13e8e953aa25d787905d28dfd9783b3bf6bb85cca2fbdbf51` |
| `PHY_INIT_AB88400` | `0x1f490` | 816 | `06eead5e4f54abd15d4086b9a9928fa5dd14bec4e82c45a44b3120fad250eafa` |
| `PHY_INIT_AB88800` | `0x1f7cc` | 816 | `fbcfd3e5ef2ac22cd067a368c9c167195329503f962431fcb7f5d5c877a033b3` |
| `PHY_INIT_AB88C00` | `0x1fb08` | 816 | `46249a54907cbfbcce5210ddc9d9d31cdb7b1ff58d5551ac63a1cc7ede558d2f` |

The bootloader copies the four PHY tables as little-endian words after its 41 ordered register writes. They are not the board’s SDD calibration. The engine initializer streams the other 430 bytes to `0x09c50008`, with bit 31 set on the first write, then issues command `0x9000`. The payload’s internal format is unknown.

The firmware began as a translation of decompiled vendor firmware. Representing vendor bytes as Rust constants does not establish redistribution rights, and the crate’s license declaration is not a separate license grant for them.
