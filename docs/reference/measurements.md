---
title: Hardware measurements
section: Hardware reference
---
# Hardware measurements

Hardware runs behind the claims in this document. These are the ones that changed a design decision.

| Observation | Result | Consequence |
| --- | --- | --- |
| Sleep duration vs request | 1,001 / 20,001 / 7 µs for 1,000 / 20,000 / 5 µs | Timer 1 is a valid wake source; below ~7 µs it is not a delay primitive |
| Wake source during RX | 3,681 of 3,707 sleeps ended with no controller source pending, while the RX producer moved | MAC FIQ (source 0x16) ends the wait; it is invisible in pending and raw registers |
| Enabling every source | 9, 24, 28 stay pending; acknowledging 28 stops RX within seconds | Leave 9, 24, 28 alone; drain the event FIFO from the main loop |
| UART IRQ 0 enabled | Ends every wait immediately | Print nothing means enable nothing |
| Replaying the routing words | No measurable change | Semantics unproven; safe to replay, not required |
| Sectioned downloader, three records | Healthy WSM idle, no leaked buffers, normal scan traffic, 7 APs retained | The host-visible file may differ from the runtime image |
| Coherent JOIN ELF (no binary splicing) | Linux reported `CW1200 WSM init done` with `XR819 open Rust WSM` | HIF startup blocker closed |
| Active-probe scan with sectioned build | First start-scan request timed out, one host buffer outstanding | Stall localised to active-probe preparation, not scan/channel handling |
| Passive sectioned build | Normal scanning, 49 frames received, 9 APs retained | Scan and channel machinery works without probes |
| JOIN + host management TX | Linux issued repeated authentication frames without `wsm_join failed`; all buffers returned | JOIN path works to authentication |
| Air capture during that JOIN | 0 frames from the station address among 1,826 valid channel-1 frames | TX publication completes but nothing reaches the air: the residual blocker |
| Invalid PHY timing values at `0x0200+0x30/0x260` | Association 13-41 s instead of ~2 s | Those words look like DTCM addresses but are timing values |

## Register-image equivalence is not behavioural equivalence
{:#s14-1}

Several bugs in this device's history had identical final register images and different behaviour. The three documented in 12.3 are the ones that were isolated. The general rule for this block: if the vendor writes a value in a particular position in a sequence, keep the position, not just the value.
