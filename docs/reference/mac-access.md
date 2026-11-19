---
title: MAC access inventory
section: Hardware reference
---
# MAC access inventory

These routines in [`src/mac.rs`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/mac.rs) select the listed constant offsets through `mac_register`. An entry records address selection, not whether the word was read or written on a particular boot. See [MAC registers](mac.html) for programming notes and computed slot addresses.

{% include registers/MacAccess.md %}
