---
title: Reading this reference
description: Conventions of the XR819 hardware reference, and how each kind of statement was established.
---
The reference describes the XR819 from the point of view of the firmware's ARM core. Addresses are the ones that core uses, not the host's SDIO register space.

## Where the facts come from

The manual separates four kinds of statement:

| Kind | Meaning | Marked as |
|---|---|---|
| Layout | Offsets, widths and bit positions from the firmware's Rust declarations. Generated, so it always matches the code. | Register tables and bit diagrams |
| Observed | A recorded read, write, value or hardware experiment, or the vendor code's behaviour. | Unmarked |
| Inferred | A role proposed from observations but not tested on its own. | <span class="tag inferred">inferred</span> |
| Hypothesis | An interpretation that still needs a distinguishing test. | <span class="tag hypothesis">hypothesis</span> |

A successful association exercises a path; it does not validate every bit programmed along it. [Open questions](unknowns.html) lists the tests that would settle the main gaps.

## Register tables

Every typed register block has a generated table with these columns:

Offset, Address
: Offset from the block base and the CPU address. Descriptor-like layouts that live in memory show sizes instead of addresses.

Register
: The Rust field name in capitals. A link means the bit layout is known and drawn below the table.

Type
: Width and the accessor the firmware declares: `RO` if it only reads the register, `WO` if it only writes it, `RW` if both. A register declared for reference but not used, such as `RAW_STATE`, keeps the accessor it would need. This records the firmware's use, not what the silicon permits. The download control block, a plain memory structure shared with the host, shows only the width; its descriptions say who writes each word.

Description
: The doc comment from the Rust declaration: usually the value the firmware writes and why.

A line under each generated table or diagram links to the Rust source file, names the declaration and gives its size. Offsets that the firmware never touches are omitted rather than marked reserved: nothing is known about them. Reset values are not known either; the tables give the values the firmware writes.

The MAC and PHY are sparse windows that the firmware reaches by offset. Their tables are maintained by hand from the code, with an Access column instead of Type: W written, R read, RW both.

Names such as `CLEAR_14`, `MASK_44` or `PARAMETER_A0` end in their offset because the register's function is not known beyond the value the firmware writes. Other names come from the firmware routine or variable that writes the register, such as `SLOT_TIMING_428` or `DEBUG_CONTEXT`; they say where the register is used, not what it does, and the description says when the function is unknown.

## Bit diagrams

<div class="bitfield"><div class="bf-grid" aria-hidden="true"><div class="bf-row bf-c1 bf-hi"><span class="bf-n">31</span><span class="bf-n">30</span><span class="bf-n">29</span><span class="bf-n">28</span><span class="bf-n">27</span><span class="bf-n">26</span><span class="bf-n">25</span><span class="bf-n">24</span><span class="bf-f" style="grid-column:span 8">FIELD</span></div><div class="bf-row bf-c2 bf-hi"><span class="bf-n">23</span><span class="bf-n">22</span><span class="bf-n">21</span><span class="bf-n">20</span><span class="bf-n">19</span><span class="bf-n">18</span><span class="bf-n">17</span><span class="bf-n">16</span><span class="bf-r" style="grid-column:span 7"></span><span class="bf-f" style="grid-column:span 1">FLAG</span></div><div class="bf-row bf-skip bf-c1 bf-lo"><span class="bf-gap">15:8 not described</span></div><div class="bf-row bf-c2 bf-lo"><span class="bf-n">7</span><span class="bf-n">6</span><span class="bf-n">5</span><span class="bf-n">4</span><span class="bf-n">3</span><span class="bf-n">2</span><span class="bf-n">1</span><span class="bf-n">0</span><span class="bf-r" style="grid-column:span 4"></span><span class="bf-f" style="grid-column:span 4">COUNT</span></div></div></div>

| Bits | Field | Description |
|---:|---|---|
| 31:24 | `FIELD` | An eight-bit field |
| 16 | `FLAG` | A one-bit field |
| 3:0 | `COUNT` | A four-bit field |
{: .bits}

A register is drawn in rows of eight bits, most significant first, with bits 31:24 and 15:8 on the left. A row with nothing described is drawn as a thin strip. Shaded cells are described fields, and the table under the diagram gives each field's bits and meaning. Hatched bits are undocumented: their function is unknown. They are not necessarily reserved, and the firmware may still write them as part of a constant, which the description then gives.

A name of the form `BITn` marks a bit the firmware programs without knowing its function. The description says when the firmware sets or clears it.

## Vendor code

The reference describes the hardware and what the firmware does with it. Which vendor routine each step reproduces is collected in the [vendor cross-reference](vendor.html).

## Notation

- Addresses are hexadecimal with an underscore between the 16-bit halves: `0x0ac0_001c`. Offsets within a block are written `+0x1c`.
- Bit ranges are written high to low: `12:1`.
- Registers are named by their Rust field in capitals, qualified by block where it helps: `PlatformControl.CONFIGURATION`, and a field `RxDma.CONTROL.ENABLE`.
- MAC registers carry their offset in the MAC window: `TSF_CONTROL` (`+0x0e34`) is at `0x09c0_0e34`.
- Field names such as `PATH0_9` combine a group with a bit number: bit 9, one of path 0's bits.
- Multi-byte values in memory are little-endian unless stated otherwise.
