---
title: Packet DMA start-up
description: How the XR819 firmware brings up receive packet DMA, the TSF, the TX rings and the MAC start-up state.
---
Once the host interface is up, the firmware programs receive DMA, the TSF block, the TX rings and the MAC's automatic responses. Register names refer to the [MAC](mac.html), [receive DMA](rx-dma.html) and [transmit ring](rings.html) pages. Three of its orderings follow the vendor even though the final register values would not show a difference; they are listed after the sequence.

## Sequence

1. Clear `PHY_DMA_8040` (`0x0ab9_8040`) and `PACKET_DMA_CFG` (MAC `+0x080c`).
2. **Receive DMA.** `CONFIGURATION_0C = 0x80` *before* `CONTROL = 0x0102_0418`, then `CONFIGURATION_1C = 7`, `PRODUCER = 0`, `CONSUMER = 0`, `CONFIGURATION_20 = 0x3000_0000`.
3. **MAC mode.** `MODE` = 0, `FILTER` = 0, `MODE_20C` = 3, and `MAC_10000` (`0x09c1_0000`, outside the MAC window) = 0. Then `BEACON_CONTROL` = `0x1030_0000`, `RESPONSE_ENABLE` = 0, `RESPONSE_MASK` = 0.
4. **TSF.** `TIMING_BLOCK` = `0x4f00_004f`, `TIMING_BLOCK_4` = `0x0909_0000`, then clear the ten words from `BEACON_TIMER` (`+0x0e08`) to `+0x0e2c`. Then `TSF_CONTROL` = 0, `TSF_LOW` = 0, `TSF_HIGH` = 0, and only then `TSF_CONTROL` = 3.
5. **Pipe scheduler.** `PIPE_CFG_60` = 2, `PIPE_CFG_80` = `0x100`, `PIPE_CFG_88` = `0xff`.
6. **TX rings.** For pipes 0 and 1, then 2 and 3: `COMMAND_BASE` (the MAC packet offset of the pipe's first command slot), `COMMAND_LENGTH` = `0x54`, `GO` = 1.
7. **Command fetch and automatic responses.** `COMMAND_FETCH` = `0xbf`. Then build the automatic response list in packet RAM: head `0x4e14_0000`, 33 entries `0x2200_0000 | offset`, where offset is the list's own MAC packet offset, and terminator `0xf000_0000`. Why every entry points back at the list is not known.
8. **Address match.** `MATCH_CONTROL` = 0, then `0x0100_0000`; the four match entries initialized disabled.
9. **Debug context.** `DEBUG_CONTEXT_4` = 0, `DEBUG_CONTEXT_8` = 0, `DEBUG_CONTEXT_C` = `0x7ff`, `DEBUG_CONTEXT_10` = 0, and `DEBUG_CONTEXT` = 0 *last*.

## Orderings that matter

Each follows the vendor's order; the reasons are untested.

| Step | Rule | Why |
|---|---|---|
| 2 | `CONFIGURATION_0C` before `CONTROL` | Writing `CONTROL` first could expose the controller to a stale configuration word and stale cursors, if `CONTROL` latches state. <span class="tag hypothesis">hypothesis</span> |
| 4 | `TSF_CONTROL` 0, then 3 | The vendor writes it this way. If the register has sticky or write-one-to-set bits, a single write of 3 is a different event. <span class="tag hypothesis">hypothesis</span> |
| 9 | Debug context base last | Writing the base first could briefly publish whatever the other words held. <span class="tag hypothesis">hypothesis</span> |

The MAC start-up below repeats steps 5 to 7; see [Transmit rings](rings.html#ring-registers).

## MAC start-up

After packet DMA the firmware writes the fixed [receive register image](mac.html#receive-register-image), initializes the receive ring, enables IRQ 26 and brings the MAC to its start-up state:

1. If `RxDma.CONTROL.ENABLE` is set, clear it and poll `ACTIVE` until it clears, bounded.
2. Collapse the FIFO: `CONSUMER = PRODUCER`, and write `CONTROL` back unchanged.
3. Clear the two duration words, initialize eleven of the thirteen response commands (all but 9 and 10) with the TBTT template, publish the pointer table at `RESPONSE_POINTER_BASE` and point all 32 slots at the idle descriptor.
4. Clear the 23 MAC `TIMING` words, wait for the MAC event FIFO to report empty, and write the receive register image again.
5. Program the TX pipes again: `COMMAND_FETCH` = 0; `PIPE_CFG_60`, `PIPE_CFG_80` and `PIPE_CFG_88` and the four rings as in steps 5 and 6 of the sequence above. Read each ring's `CURSOR_AND_PENDING_MASK` to seed the firmware's slot cursor for that pipe. Then `COMMAND_FETCH` = `0xbf`, and rebuild the automatic response list as in step 7. Clear the interface metadata word in packet RAM.

Finally `PACKET_CONTROLLER` (MAC `+0x1000`) = 1 enables the packet controller, and the firmware sends the [startup indication](wsm.html#startup-indication).
