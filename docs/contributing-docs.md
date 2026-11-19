---
title: Editing the manual
section: Lab notes
---
# Editing the manual

Edit the Markdown page in `docs/` or `docs/reference/` for behavior, measurements and interpretations. Give a source routine, firmware revision, or board run for new claims. Note what the evidence doesn't establish.

For offsets, widths or array counts, edit the corresponding `register_structs!` or `register_bitfields!` declaration under `src/`. The generator reads Rust syntax, including ARM-only definitions. The address map uses the firmware's base constants and MMIO windows. The PHY initialization sequence comes from `src/loader.rs` in write order. Don't edit generated tables or create a parallel YAML register map.

Run `cargo xtask docs` from the repository root to regenerate the Markdown fragments under `docs/_includes/registers/`. A page includes a fragment by name, for example `{% raw %}{% include registers/AesRegisters.md %}{% endraw %}`. Generated files aren't committed. Some MAC and RF registers are programmed by sparse or computed addresses rather than typed register blocks; their access and interpretation are documented in Markdown, not inferred from gaps in a struct.

For a new register block, connect its layout and base symbol in `xtask/src/docs.rs`, include its table on the relevant page and add that page to `docs/_data/navigation.yml`. Keep old section routes in `docs/_data/legacy.json` when moving a chapter. CI builds the site for pull requests; only `main` can deploy it.
