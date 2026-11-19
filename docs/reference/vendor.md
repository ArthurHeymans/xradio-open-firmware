---
title: Vendor cross-reference
description: Which vendor firmware routine each XR819 firmware step reproduces, for checking the reference against the vendor image.
---
This page maps the manual's steps and registers to vendor code, for anyone checking a claim against the vendor image. Addresses are offsets in that image. Names such as `mac_irq_handler` are annotations from the reverse engineering, not vendor symbols; `FUN_` names were never given one.

## Start-up

| Step | Firmware function | Vendor routine |
|---|---|---|
| [Configuration word](bringup.html#configuration-word) | `platform::prepare_main_control` | `0x0001_64c8` |
| [Remap, memory and interrupts](bringup.html#remap-memory-and-interrupts) | `platform::prepare_memory_and_interrupts` | `0x0000_09e0`; remap sampling `0x0001_5944` |
| [Debug console](bringup.html#debug-console) | `platform::prepare_high_platform_support` | `0x0000_0a74`, then `0xfff0_19aa` |
| [DMA and clocks](bringup.html#dma-and-clocks) | `platform::prepare_dma_and_clocks` | `0x0000_0bb0`, `0x0000_58b6` |
| [Wait for the host](hif-flow.html#wait-for-the-host) | `platform::try_activate_hif` | `0x0000_0ab4` |
| Timer and AES interrupts | `platform::register_post_activation_interrupts` | `0x0000_0c38`, `0x0000_0a88`; IRQ 21 at `0x0000_0b88` |
| [Publish the rings](hif-flow.html#publish-the-rings) | `hif::Transport::initialize` | `0x0000_094c` |
| [Packet DMA](bringup-dma.html) | `platform::prepare_packet_dma` | `0x0000_00f6`, with `0x0000_00bc`, `0x0000_f608`, `0x0000_f4f4`, `0x0000_0044` |
| [Receive register image](mac.html#receive-register-image) | `platform::prepare_mac_receive_hardware` | `rx_subsystem_init` |
| [MAC start-up](bringup-dma.html#mac-start-up) | `mac::initialize_startup_state` | `0x0000_014c`, within `0x0001_6d24` |
| Packet controller | `platform::enable_packet_controller` | `0x0000_0c80` |
| [Startup indication](wsm.html#startup-indication) | `wsm::StartupIndication` | `0x0001_58a8` |
{: .compact}

The whole sequence is `main` in `src/bin/hif_startup.rs`.

## Routines and handlers

| Subject | Vendor routine |
|---|---|
| MAC event decoding ([event word](mac.html#event-word)) | `mac_irq_handler`, `0x0000_9eb4` |
| Pipe service ([`PIPE_SCHEDULER`](mac.html#pipe_scheduler)) | `mac_pipe_irq_service`, `0x0000_9b6e`–`0x0000_9c28` |
| Watchdog pause of `COMMAND_FETCH` bits 5:2 | `mac_rx_pause_briefly`, `0x0000_030a`; the name suggests these bits gate reception |
| TX ring start-up ([Transmit rings](rings.html#ring-registers)) | `mac_hw_init_pipes` |
| Output descriptor reclaim ([Host interface](hif.html#ownership)) | `hif_tx_confirm_drain` |
| AES completion, IRQ 18 and 20 | `enc_xfer_complete`, `0x0000_ea08` |
| IRQ 26, TSF | `0x0000_7e3c` |
| IRQ 27, TX error | `event_send_error_0x34` |
| UART interrupts 0 and 2 | `0xfff0_19d8`, `0xfff0_19e0` |
| Beacon arming and disarming | `tsf_align_to_beacon_interval`, `0x0000_2d38`; `mac_disable_beacon_hw`, `0x0001_0172` |
| BSSID programming | `mac_program_bssid`, `0x0001_0b1a` |
| Command descriptors | `txp_desc_emit` |
| TSF set-up, 0 then 3 | `tsf_hw_init` |
| Stopping the PHY's current operation | PHY command 7, `pac_phy_stop_op`; in this firmware it only resets the PHY operation state |
| Debug context, base last | `hif_dbg_ctx_init` |
| Interrupt routing words | Instruction stream at `0x0001_64fa` |
{: .compact}

## Interrupt routing words

The vendor writes nine words in the PHY window, after subsystem initialization and before it enables interrupts. The firmware does not write them: replaying them changed nothing measurable, and their meaning is unknown. The loader writes two more words in the same window, `0x0abb_0008` = `0x0000_026d` and `0x0abb_0078` = `0x0048_0035`.

{% include registers/InterruptRouting.md %}

## AES transfer classes

The vendor queues AES work through a transfer-class table in DTCM at `0x0400_0804`. Classes 6 (flags `0x84`) and 7 (flags `0x80`) are CCMP encryption and decryption; classes 8 to 10 serve protected management frames and BIP. The vendor completion handler clears the global owner, checks status bit 0 for classes 6 to 9, moves the descriptor to a deferred-completion list, starts the next queued operation and runs callbacks outside interrupt context.

## Where the firmware touches each MAC offset

Generated from the constant offsets that functions in [`src/mac.rs`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/mac.rs) pass to `mac_register`. An entry records that the function selects the address, not whether it reads or writes it. Computed slot addresses and accesses from other modules are not included.

<details markdown="1">
<summary>Show the table</summary>

{% include registers/MacAccess.md %}

</details>
