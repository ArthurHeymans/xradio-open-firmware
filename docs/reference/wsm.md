---
title: WSM message identifiers
section: Hardware reference
---
# WSM message identifiers

WSM is the protocol between `cw1200` and the XR819. The [host interface](hif.html) moves each message through packet RAM. Its header is four bytes: a little-endian length followed by a little-endian identifier. The values below are base IDs, without the sequence or link bits.

{% include registers/Messages.md %}

The list is generated from the named message constants in [`src/wsm.rs`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/wsm.rs). A command confirmation and a later completion indication are distinct messages: for example, a successful scan command doesn't mean scanning has finished. Generic confirmations may be derived from a request ID rather than declared separately. MIB numbers belong to a separate namespace.
