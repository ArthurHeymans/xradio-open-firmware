---
title: Starting packet DMA
section: Hardware reference
---
# Starting packet DMA

`0x000009ac` is the routine that makes packet memory usable and then builds the startup indication. Its callees run in this order, and the ordering constraints inside them are documented per call:

```
0x000009ac
  ├─ 0x0000094c   HIF buffer geometry
  ├─ 0x000000f6   packet DMA + MAC pipes
  │    ├─ 0x000000bc   register block
  │    ├─ 0x0000f608   MAC timing block at 0x09c00e00
  │    ├─ 0x0000f4f4   automatic response list, command fetch
  │    ├─ 0x00000044   match entries, debug context
  │    ├─ 0x0000fcfe   per-interface context
  │    ├─ 0x000004c6   ...
  │    ├─ IRQ 27 registration
  │    └─ IRQ 26 registration
  ├─ 0x00016d24   ...
  ├─ 0x0000014c   MAC startup state
  ├─ startup indication construction (0x000158a8)
  └─ 0x000009ac tail
```

### Packet-DMA initialization order (as implemented)

1. `0x0ab9_8040 = 0`, `0x09c0_080c = 0`.
2. RX DMA: `configuration_0c = 0x80` *before* `control = 0x0102_0418`, then `configuration_1c = 7`, producer and consumer 0, `configuration_20 = 0x3000_0000`.
3. MAC timing: `0x09c0_0200 = 0`, `0x0204 = 0`, `0x020c = 3`, `0x09c1_0000 = 0`, then the response block `0x0a00 = 0x1030_0000`, `0x0a04 = 0`, `0x0a1c = 0`.
4. TSF block: `0x0e00 = 0x4f00_004f`, `0x0e04 = 0x0909_0000`, offsets `0x08..0x2c` cleared, then `0x0e34 = 0` and only then `0x0e34 = 3`.
5. `0x0e60 = 2`, `0x0e80 = 0x100`, `0x0e88 = 0xff`.
6. For pipe pairs (0,1) and (2,3): command base, command length `0x54`, GO = 1.
7. `0x0e8c = 0xbf` (command fetch on), then the automatic response list: head `0x4e14_0000`, 33 entries `0x2200_0000 | offset`, terminator `0xf000_0000`.
8. `0x09c0_1300 = 0` then `0x0100_0000`; four match entries initialised to disabled.
9. Debug context: parameters written first, base `0x1400 = 0` *last*.
10. MAC receive hardware: the fixed register image at `0x09c0_0000..0x00d4`, `0x09c0_0060..0x00f0`, and the slot-timing block at `0x09c0_0200`.
11. Packet controller: `0x09c0_1000 = 1`.

**Three orderings that are not interchangeable even though the final register image is identical.**

- RX DMA `+0x0c` before `+0x00`: writing control first exposes the controller to a stale configuration word across the transition.
- `0x0e34` zero then three: if the register has sticky or write-one-to-set bits, a direct write of 3 is a different event.
- Debug context `+0x00` last: writing the base first transiently publishes whatever `+0x04..+0x10` held.

Each of these was a real bug that left the final values correct.
