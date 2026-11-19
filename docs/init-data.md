---
title: Vendor initialization data
description: The five byte arrays the XR819 firmware takes from the vendor image, where they come from and what is known about them.
---
[`src/init_data.rs`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/init_data.rs) holds five byte arrays copied from the vendor runtime image. They are literal constants, not generated at build time. Their offsets locate them in the vendor `fw_xr819.bin` with SHA-256 `3e2462d476c9dfcb907cda1ba81d0a6d1bbee5e3207bdc1911ec5042d96fdfca`.

| Constant | File offset | Bytes | SHA-256 of the bytes |
|---|---:|---:|---|
| `ENGINE_MODE1_INIT` | `0x1d6c4` | 430 | `211ad6ec637a780d770998fbe76bcefa1b127506ef8e0f9f09539da32d881b4c` |
| `PHY_INIT_AB81000` | `0x1f084` | 1024 | `2ec66edfbaf0edc13e8e953aa25d787905d28dfd9783b3bf6bb85cca2fbdbf51` |
| `PHY_INIT_AB88400` | `0x1f490` | 816 | `06eead5e4f54abd15d4086b9a9928fa5dd14bec4e82c45a44b3120fad250eafa` |
| `PHY_INIT_AB88800` | `0x1f7cc` | 816 | `fbcfd3e5ef2ac22cd067a368c9c167195329503f962431fcb7f5d5c877a033b3` |
| `PHY_INIT_AB88C00` | `0x1fb08` | 816 | `46249a54907cbfbcce5210ddc9d9d31cdb7b1ff58d5551ac63a1cc7ede558d2f` |
{: .compact}

## PHY tables

The four `PHY_INIT_` tables are copied by the bootloader; the names give their destinations. See [loader tables](reference/phy.html#loader-tables).

## AES initialization stream

`ENGINE_MODE1_INIT` is the stream the AES engine receives during the firmware's [start-up self-test](reference/aes.html#start-up-self-test). Its format is unknown.

## Licensing

The firmware began as a translation of decompiled vendor firmware. Representing vendor bytes as Rust constants does not establish a right to redistribute them, and the crate's license declaration is not a separate license grant for them.
