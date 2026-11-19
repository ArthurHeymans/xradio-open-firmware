---
title: Editing this manual
description: How the XR819 manual is built, which parts are generated from Rust, and how to change it.
---
The manual is a Jekyll site in `docs/`, built by GitHub Pages. Pull requests build it in CI; only `main` deploys it.

## Generated and written parts

| What | Source of truth | Edit |
|---|---|---|
| Register tables, bit diagrams, offsets, widths | `register_structs!` and `register_bitfields!` declarations under `src/` | The Rust declaration and its doc comments |
| Register descriptions in those tables | `///` doc comments on fields and bitfields | The Rust doc comment |
| Memory map of typed blocks | Base constants and `RegisterWindow` statics | `src/platform.rs`, `src/mmio.rs` |
| Loader register writes | `VENDOR_REGISTER_INITIALIZATION` in `src/loader.rs` | The Rust table |
| WSM message IDs | `_ID` constants in `src/wsm.rs` | The Rust constants |
| MAC, PHY and other untyped registers | `docs/_data/mac.yml`, `phy.yml` and `platform.yml`, rendered by `_includes/register-rows.html` | The data file |
| Register index | All of the above, assembled by `cargo xtask docs` | Nothing |
| Behaviour, sequences, measurements | Markdown pages | The page |

Do not edit the generated fragments or keep a parallel register list in Markdown. Registers without a typed layout, chiefly the MAC and PHY windows, are the exception: the firmware reaches them by offset, so they live in the data files above, which also feed the register index. A row there takes `offset` (MAC) or `address`, `name`, `use` (W, R or RW) and `text`, and optionally `bits`, `tag`, `count` and `stride` for a range, `page` and `block`.

## Building locally

```sh
cargo xtask docs                    # writes docs/_includes/registers/
cd docs && jekyll serve --baseurl ""
```

`cargo xtask docs` parses the Rust sources, including ARM-only items, and fails on anything it cannot place: an unknown register type, overlapping fields, or a register block without a base address. It writes the fragments and `docs/_data/registers.json`; neither is committed. CI then builds the site and runs `.github/scripts/check-site.py`, which fails on broken links or anchors, duplicate IDs and pages without a title.

## Conventions

- Give new claims a source: a routine, a firmware revision or a board run. Say what the evidence does not establish.
- Mark interpretations with <span class="tag inferred">inferred</span> or <span class="tag hypothesis">hypothesis</span>: `<span class="tag inferred">inferred</span>` in Markdown, or `(inferred)` in a Rust doc comment. Unmarked statements are observed.
- Keep vendor addresses and reverse-engineering notes on the [vendor cross-reference](reference/vendor.html), not in register descriptions.
- State each rule once, on the page that owns it, and link to it elsewhere.
- Name bits whose function is unknown `BITn`, and describe what the firmware does with them.
- Write addresses as `0x0ac0_001c` and bit ranges high to low.

## Adding a register block

1. Declare its layout with `register_structs!`, and its known bits with `register_bitfields!`, attaching each group to its register as `ReadWrite<u32, Group::Register>`. Use `ReadOnly` or `WriteOnly` when the firmware only reads or only writes a register: the Type column shows it. Document fields and bits with `///` comments.
2. Connect the layout to its base-address constant in `BLOCKS` in `xtask/src/docs.rs`.
3. Include the fragments on the block's page: `{% raw %}{% include registers/Block.md %}{% endraw %}` for the table, and one include per bitfield group under a heading named after the register. The register table links to the diagram's anchor, `#group-name-bits`.
4. Add the page's URL to `docs/_data/navigation.yml`. The sidebar shows the page's `title` (or `nav_title`), and its order is also the order of the previous and next links.

When a page moves, add a stub with `layout: redirect` and `redirect_to:` at the old path so existing links keep working.
