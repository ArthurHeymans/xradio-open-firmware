---
title: Board test reports
description: Kernel and firmware revisions tested together on the Orange Pi Zero, and the results.
---
Each run below records the exact firmware and kernel revisions, what was tested and what happened. All runs used an Orange Pi Zero over SDIO, with an Intel adapter (`wlp4s0`) as the peer. The first two are connectivity checks only.

## 2026-11-19: driver series for review

| Item | Revision |
|---|---|
| Firmware | `58f32d3`; bootloader SHA-256 `05ba3714e15477ed72b38ec31ad0792f731cd74728b032cb72ce5229c53f0b99` (as in the first run), runtime `bd43771e0bcfea9c83e29f3e8f8776edccf46f6839e3c3c32bfdb5a6cfa80d19`, `sdd_xr819.bin` `84d3fb3ca8e5d25a0c113a5063bccbeb5b53da230a0afa236b5b625f37db5161` |
| Kernel | Ten-patch series on Torvalds master `4405c18c95d2`, tip [`bb557dc24a46`](https://github.com/ArthurHeymans/linux/commit/bb557dc24a46), reported as 7.4.0-rc3 |
| Peer | The Intel adapter on channel 6; WPA2-PSK with CCMP throughout |
{: .compact}

Throughput is one TCP stream from a Python sender, 15 to 60 seconds per run.

**Station.** Every association completed in 1.1 to 1.3 seconds, one in 6.9. Throughput was 17 to 26 Mbit/s from the peer to the board and 15 to 24 Mbit/s back.

**Soak.** For 10 minutes the peer and the board took turns sending for a minute each: 728 MB to the board, 640 MB from it. A concurrent ping got 1,182 of 1,214 replies, with round trips up to 2.1 seconds while the link was saturated. The link stayed associated and the kernel logged no errors.

**Rebind.** Unbind and bind while associated, then a new association: 4 cycles with the SDIO interrupt and 5 with a GPIO interrupt. All associated; 20 pings right after lost at most one, except once 6 of 20, which recovered to 47 of 50 a minute later. Before [`655e896fe871`](https://github.com/ArthurHeymans/linux/commit/655e896fe871), every unbind of an associated board logged `CMD req (0x0009) stuck in firmware, killing BH`: the driver freed its interrupt before mac80211 removed the interface, so the confirmations of the commands sent during removal were lost. Associations that failed after a rebind early in this run came from the test script, which left an extra `wpa_supplicant` on the interface; the retry needed in the previous run may have had the same cause.

**Access point.** hostapd started without the failed `START` confirmation of the previous run: `58f32d3` accepts the listen-mode `START` that the driver sends first. The peer completed the four-way handshake. Pings with a 1,200-byte payload: 46 of 50 to the board and 50 of 50 back in one run, 30 of 30 and 28 of 30 in another. Throughput was 11.1 Mbit/s from the board and 9.5 Mbit/s to it.

**GPIO interrupt.** A device-tree overlay giving the XR819 node `compatible = "xradio,xr819"` and an interrupt on PG10 made the driver use the GPIO interrupt instead of the SDIO one. Throughput was 25 Mbit/s to the board and 16 Mbit/s from it.

Earlier runs used builds that lacked the listen-mode change or carried five behaviour-neutral lint rewrites, which change the image hashes. The images listed above were retested afterwards for two rebinds, throughput in both directions and an access-point start.

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
