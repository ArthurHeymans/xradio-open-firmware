---
title: Board test reports
description: Kernel and firmware revisions tested together on the Orange Pi Zero, and the results.
---
Each run below records the exact firmware and kernel revisions, what was tested and what happened. All runs used an Orange Pi Zero over SDIO, with an Intel adapter (`wlp4s0`) as the peer. They are connectivity checks, not throughput, loss or endurance measurements.

## 2026-11-19: rebind and WPA2 retest

| Item | Revision |
|---|---|
| Firmware | `f9e52aa`; runtime SHA-256 `8069466211146778955ecf17545dcf45259678b1726779b5eaa40ac75790c3ed`, bootloader as in the first run |
| Kernel | Six-patch series on Torvalds master `4405c18c95d2`, tip [`e4bcf24ebf56`](https://github.com/ArthurHeymans/linux/commit/e4bcf24ebf56), then `476daf13af49` (not published; the same reset is part of [`2bd278da55ca`](https://github.com/ArthurHeymans/linux/commit/2bd278da55ca) on the review branch) |
{: .compact}

**WPA2.** With `e4bcf24ebf56` the board exchanged WPA2-CCMP traffic with the peer as station and as access point, and unbound without an oops.

**Rebind.** Bind then failed with `-EINVAL`: the XR819 was still in queue mode, so the driver could not start another firmware download. `476daf13af49` resets only the XR819 SDIO card on probe. With it, three consecutive unbind/bind cycles passed, and a fourth passed after rebooting into the revised module. Both firmware images read back unchanged.

**After rebind.** The first station association did not complete within 35 seconds; the retry did. Pings with a 1,200-byte payload: 19 of 20 from board to peer, 20 of 20 back. As an access point, 10 of 10 each way. The kernel still logged a failed WSM confirmation for request `0x0017` (`START`).

## 2026-11-19: first boot with the open firmware

| Item | Revision |
|---|---|
| Firmware | `f53c5d4`; bootloader SHA-256 `05ba3714e15477ed72b38ec31ad0792f731cd74728b032cb72ce5229c53f0b99`, runtime `2c0226d2bca44e12fbcdc27fa9e57c70aaaba313ecd1c6d6924b2db1f578c21a` |
| Kernel | [`8b3b8d68f31d`](https://github.com/ArthurHeymans/linux/commit/8b3b8d68f31d) on Torvalds master `4405c18c95d2`, built with SDIO and SPI |
| Checks | 267 Rust tests, 44 Python tests, ARM stack check and both image builds passed; a fresh checkout produced identical images |
{: .compact}

The probe reported `XR819 open Rust native`.

**Station.** WPA2-PSK/CCMP completed against the peer. Each end received 9 of 10 pings.

**Access point.** hostapd completed the four-way handshake. The board lost its test IPv4 address on the interface mode change; after reassigning it, both ends received 20 of 20 pings with a 1,200-byte payload.

**Unbind.** SDIO unbind faulted in `cw1200_debug_release`: unregistering the wiphy had already removed its debugfs subtree. Kernel [`e4bcf24ebf56`](https://github.com/ArthurHeymans/linux/commit/e4bcf24ebf56) drops the second removal; the retest above uses it.
