//! Render the subset of the tock-registers layout grammar used by XR819.
//! Parse Rust syntax (including ARM-only items); never derive register widths from gaps.
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, fmt::Write, fs, path::Path};
use syn::{
    Attribute, Expr, Ident, Item, Lit, LitInt, Token, Type, Visibility, braced, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

mod access;
mod values;

const REPOSITORY: &str = "https://github.com/ArthurHeymans/xradio-open-firmware/blob/main";

/// Joins `///` lines into one Markdown paragraph that is safe in a table cell.
fn doc(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for a in attrs {
        if !a.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &a.meta
            && let Expr::Lit(l) = &nv.value
            && let Lit::Str(s) = &l.lit
        {
            lines.push(s.value().trim().to_owned());
        }
    }
    let text = lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        // Rustdoc intra-doc links read as plain code here.
        .replace("[`", "`")
        .replace("`]", "`");
    // Table cells: kramdown keeps the backslash of `\|` inside code spans, so
    // render code as HTML, where a pipe can be an entity.
    let mut out = String::new();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            out.push_str("<code>");
            for c in part.chars() {
                match c {
                    '<' => out.push_str("&lt;"),
                    '>' => out.push_str("&gt;"),
                    '&' => out.push_str("&amp;"),
                    '|' => out.push_str("&#124;"),
                    c => out.push(c),
                }
            }
            out.push_str("</code>");
        } else {
            out.push_str(&part.replace('|', "\\|"));
        }
    }
    for kind in ["inferred", "hypothesis"] {
        out = out.replace(
            &format!("({kind})"),
            &format!("<span class=\"tag {kind}\">{kind}</span>"),
        );
    }
    out
}
/// `RxDmaControl` -> `rx-dma-control`, the anchor of a rendered bitfield.
fn anchor(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}
/// `0x60000` -> `0x6_0000`: hex grouped by four digits from the right.
fn hex_grouped(v: u32) -> String {
    let digits = format!("{v:x}");
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 4 == 0 {
            out.push('_');
        }
        out.push(c);
    }
    format!("0x{out}")
}
/// `0x0ac0001c` -> `0x0ac0_001c`, the spelling used in the prose.
fn hex32(v: u32) -> String {
    format!("0x{:04x}_{:04x}", v >> 16, v & 0xffff)
}
struct Field {
    offset: u32,
    name: String,
    ty: Type,
    doc: String,
}
struct Block {
    name: String,
    fields: Vec<Field>,
    size: u32,
}
impl Parse for Block {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        input.call(Attribute::parse_outer)?;
        input.parse::<Visibility>()?;
        let name = input.parse::<Ident>()?.to_string();
        let rows;
        braced!(rows in input);
        let mut fields = Vec::new();
        loop {
            let attrs = rows.call(Attribute::parse_outer)?;
            let row;
            parenthesized!(row in rows);
            let offset = row.parse::<LitInt>()?.base10_parse()?;
            row.parse::<Token![=>]>()?;
            if row.peek(Token![@]) {
                row.parse::<Token![@]>()?;
                if row.parse::<Ident>()? != "END" || !row.is_empty() {
                    return Err(row.error("expected @END"));
                }
                let _ = rows.parse::<Option<Token![,]>>()?;
                if !rows.is_empty() {
                    return Err(rows.error("field after @END"));
                }
                return Ok(Block {
                    name,
                    fields,
                    size: offset,
                });
            }
            row.parse::<Visibility>()?;
            let field = row.parse::<Ident>()?.to_string();
            if row.peek(Token![:]) {
                row.parse::<Token![:]>()?;
                fields.push(Field {
                    offset,
                    name: field,
                    ty: row.parse()?,
                    doc: doc(&attrs),
                });
            } else if !field.starts_with('_') {
                return Err(row.error("untyped non-padding field"));
            }
            if !row.is_empty() {
                return Err(row.error("unsupported register field"));
            }
            rows.parse::<Token![,]>()?;
        }
    }
}
/// `register_bitfields!` or `tock_registers::register_bitfields!`.
fn macro_named(m: &syn::ItemMacro, name: &str) -> bool {
    m.mac.path.segments.last().is_some_and(|s| s.ident == name)
}
fn number(e: &Expr) -> Option<u32> {
    if let Expr::Lit(l) = e
        && let Lit::Int(i) = &l.lit
    {
        i.base10_parse().ok()
    } else {
        None
    }
}
/// What one register-struct field is: its rendered type, size and bitfield group.
struct Shape {
    text: String,
    size: u32,
    bits: Option<String>,
}
#[derive(Default)]
struct Model {
    bits: BTreeMap<String, String>,
    layouts: BTreeMap<String, (Block, String)>,
    constants: BTreeMap<String, u32>,
}
impl Model {
    fn add(&mut self, path: &str, src: &str) -> Result<()> {
        for item in syn::parse_file(src)?.items {
            match item {
                Item::Macro(m) if macro_named(&m, "register_structs") => {
                    for block in m
                        .mac
                        .parse_body_with(Punctuated::<Block, Token![,]>::parse_terminated)?
                    {
                        let key = block.name.clone();
                        ensure!(
                            self.layouts.insert(key, (block, path.into())).is_none(),
                            "duplicate register layout in {path}"
                        );
                    }
                }
                Item::Macro(m) if macro_named(&m, "register_bitfields") => {
                    for group in m.mac.parse_body::<values::BitGroups>()?.0 {
                        let markdown = group.markdown(path);
                        ensure!(
                            self.bits.insert(group.name, markdown).is_none(),
                            "duplicate bitfield group"
                        );
                    }
                }
                Item::Const(c) => {
                    if let Some(v) = number(&c.expr) {
                        self.constants.insert(format!("{path}:{}", c.ident), v);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn shape(&self, ty: &Type) -> Result<Shape> {
        match ty {
            Type::Array(a) => {
                let count = number(&a.len).context("expected literal array length")?;
                let element = self.shape(&a.elem)?;
                Ok(Shape {
                    text: format!("{count} × {}", element.text),
                    size: count
                        .checked_mul(element.size)
                        .context("array size overflow")?,
                    bits: element.bits,
                })
            }
            Type::Path(p) => {
                let seg = p.path.segments.last().context("empty field type")?;
                let name = seg.ident.to_string();
                if let Some((block, _)) = self.layouts.get(&name) {
                    return Ok(Shape {
                        text: format!("[{name}](#{})", anchor(&name)),
                        size: block.size,
                        bits: None,
                    });
                }
                let access = match name.as_str() {
                    "ReadWrite" => "RW",
                    "ReadOnly" => "RO",
                    "WriteOnly" => "WO",
                    _ => bail!("unknown register type {name}"),
                };
                let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
                    bail!("missing register width")
                };
                let mut args = args.args.iter();
                let Some(syn::GenericArgument::Type(Type::Path(t))) = args.next() else {
                    bail!("missing register width")
                };
                let id = t.path.get_ident().context("missing register width")?;
                let size = match id.to_string().as_str() {
                    "u32" => 4,
                    "u16" => 2,
                    "u8" => 1,
                    _ => bail!("unsupported register width {id}"),
                };
                // `ReadWrite<u32, Group::Register>` names the bitfield group.
                let bits = match args.next() {
                    Some(syn::GenericArgument::Type(Type::Path(g))) => {
                        let segs = &g.path.segments;
                        ensure!(
                            segs.len() == 2 && segs[1].ident == "Register",
                            "expected Group::Register"
                        );
                        let group = segs[0].ident.to_string();
                        ensure!(self.bits.contains_key(&group), "missing bitfield {group}");
                        Some(group)
                    }
                    None => None,
                    _ => bail!("unsupported register argument"),
                };
                Ok(Shape {
                    text: format!("{id} {access}"),
                    size,
                    bits,
                })
            }
            _ => bail!("unsupported field type"),
        }
    }
    fn table(&self, name: &str, base: Option<u32>) -> Result<String> {
        let (block, path) = self
            .layouts
            .get(name)
            .with_context(|| format!("missing {name}"))?;
        let mut out = format!(
            "| Offset | {} | Register | Type | Description |\n|---:|---:|---|---|---|\n",
            if base.is_some() { "Address" } else { "Size" }
        );
        let mut last = 0;
        for f in &block.fields {
            let shape = self.shape(&f.ty)?;
            ensure!(
                shape.size > 0 && f.offset >= last,
                "overlapping fields in {name}"
            );
            last = f.offset.checked_add(shape.size).context("field overflow")?;
            ensure!(last <= block.size, "field exceeds {name}");
            let address = if let Some(b) = base {
                format!(
                    "`{}`",
                    hex32(b.checked_add(f.offset).context("address overflow")?)
                )
            } else {
                format!("{} B", shape.size)
            };
            let row = format!("{}-{}", anchor(name), f.name.replace('_', "-"));
            let mut register = format!("`{}`", f.name.to_uppercase());
            if let Some(group) = &shape.bits {
                register = format!("[{register}](#{}-bits)", anchor(group));
            }
            writeln!(
                out,
                "| `+0x{:02x}` | {address} | <span id=\"{row}\"></span>{register} | {} | {} |",
                f.offset, shape.text, f.doc
            )?;
        }
        writeln!(
            out,
            "{{: .registers}}\n\n<p class=\"source\">Source: <a href=\"{REPOSITORY}/{path}\"><code>{name}</code></a> · {} bytes{}</p>",
            block.size,
            if base.is_some() {
                ""
            } else {
                " · offsets are relative to the structure"
            }
        )?;
        Ok(out)
    }
}
const PLAT: &str = "src/platform.rs";
const HIF: &str = "src/hif.rs";
const IRQ: &str = "src/interrupts.rs";
// Join layout names to address *symbols*, not copies of hardware addresses.
const BLOCKS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "AesRegisters",
        PLAT,
        "AES_REGISTER_BASE",
        "aes",
        "AES/CCMP engine",
    ),
    (
        "RxShared",
        HIF,
        "RX_DESCRIPTOR_BASE",
        "hif",
        "Host interface, input ring",
    ),
    (
        "HifShared",
        HIF,
        "TX_DESCRIPTOR_BASE",
        "hif",
        "Host interface, output ring and control",
    ),
    (
        "InterruptController",
        IRQ,
        "BASE",
        "interrupts",
        "Interrupt controller",
    ),
    (
        "SystemControl",
        PLAT,
        "SYSTEM_CONTROL_BASE",
        "system",
        "System control",
    ),
    (
        "ClockReset",
        PLAT,
        "CLOCK_RESET_BASE",
        "clock",
        "Clock and reset",
    ),
    ("Timers", PLAT, "TIMERS_BASE", "timers", "Timers"),
    (
        "PlatformControl",
        PLAT,
        "PLATFORM_CONTROL_BASE",
        "platform",
        "Platform control",
    ),
    (
        "RxDma",
        PLAT,
        "MAC_REGISTER_BASE",
        "rx-dma",
        "Receive packet DMA",
    ),
    (
        "InterruptRouting",
        PLAT,
        "INTERRUPT_ROUTING_BASE",
        "vendor",
        "Interrupt routing words",
    ),
];
pub fn generate(root: &Path) -> Result<()> {
    let mut model = Model::default();
    let sources = [
        PLAT,
        HIF,
        IRQ,
        "src/crypto/engine.rs",
        "src/radio.rs",
        "src/mac.rs",
        "src/tx/event.rs",
        "src/tx/pipe.rs",
        "src/phy/calibration.rs",
        "src/phy/channel.rs",
    ];
    for path in sources {
        model
            .add(path, &fs::read_to_string(root.join(path))?)
            .with_context(|| format!("reading {path}"))?;
    }
    let dir = root.join("docs/_includes/registers");
    fs::create_dir_all(&dir)?;
    let mut index = values::windows(&fs::read_to_string(root.join("src/mmio.rs"))?)?;
    for &(name, file, symbol, page, title) in BLOCKS {
        let mut addr = *model
            .constants
            .get(&format!("{file}:{symbol}"))
            .with_context(|| format!("missing {symbol}"))?;
        if name == "RxDma" {
            addr = addr
                .checked_add(
                    *model
                        .constants
                        .get("src/platform.rs:RX_DMA_OFFSET")
                        .context("missing RX_DMA_OFFSET")?,
                )
                .context("RX DMA address overflow")?;
        }
        fs::write(
            dir.join(format!("{name}.md")),
            model.table(name, Some(addr))?,
        )?;
        let size = model.layouts[name].0.size;
        index.retain(|w| w.base != addr);
        index.push(values::Window {
            base: addr,
            size,
            title: title.into(),
            page: page.into(),
        });
    }
    let relative = ["Descriptor", "SlotHeader"];
    for name in relative {
        fs::write(dir.join(format!("{name}.md")), model.table(name, None)?)?;
    }
    ensure!(
        BLOCKS.len() + relative.len() == model.layouts.len(),
        "new register block needs an address or relative layout"
    );
    for (name, table) in &model.bits {
        fs::write(dir.join(format!("{name}.md")), table)?;
    }
    let (loader, loader_writes) = values::loader(&fs::read_to_string(root.join("src/loader.rs"))?)?;
    fs::write(dir.join("LoaderWrites.md"), loader)?;
    fs::write(
        dir.join("Mibs.md"),
        values::mibs(&fs::read_to_string(root.join("src/wsm.rs"))?)?,
    )?;
    fs::write(
        dir.join("Messages.md"),
        values::messages(&fs::read_to_string(root.join("src/wsm.rs"))?)?,
    )?;
    index.sort_by_key(|w| w.base);
    let mut out = String::from("| Base | Extent | Block |\n|---:|---:|---|\n");
    for w in index.iter().filter(|w| w.page != "vendor") {
        writeln!(
            out,
            "| `{}` | `{}` | [{}]({}.html) |",
            hex32(w.base),
            hex_grouped(w.size),
            w.title,
            w.page
        )?;
    }
    writeln!(out, "{{: .registers .compact}}")?;
    fs::write(dir.join("map.md"), out)?;
    fs::write(
        dir.join("MacAccess.md"),
        access::mac(
            &fs::read_to_string(root.join("src/mac.rs"))?,
            *model
                .constants
                .get("src/platform.rs:MAC_REGISTER_BASE")
                .context("missing MAC base")?,
        )?,
    )?;
    let (image, image_writes) = values::register_image(
        &fs::read_to_string(root.join(PLAT))?,
        "prepare_mac_receive_hardware",
    )?;
    fs::write(dir.join("MacReceiveImage.md"), image)?;
    const DOWNLOAD_CONTROL: u32 = 0x0900_ff80;
    let (table, download) = access::c_layout(
        "src/download.rs",
        &fs::read_to_string(root.join("src/download.rs"))?,
        "Control",
        Some(DOWNLOAD_CONTROL),
    )?;
    fs::write(dir.join("DownloadControl.md"), table)?;
    let ring_base = *model
        .constants
        .get("src/platform.rs:TX_RING_REGISTER_BASE")
        .context("missing TX ring base")?;
    let (table, ring) = access::c_layout(
        "src/tx/pipe.rs",
        &fs::read_to_string(root.join("src/tx/pipe.rs"))?,
        "TxHardwareRingLayout",
        Some(ring_base),
    )?;
    fs::write(dir.join("TxRing.md"), table)?;

    // Register index data: every addressed register, for the index page.
    // (address, name, block, url); named rows win over unnamed words.
    let mut registers: Vec<(u32, String, String, String)> = Vec::new();
    for &(name, file, symbol, page, title) in BLOCKS {
        let (block, _) = &model.layouts[name];
        let mut base = model.constants[&format!("{file}:{symbol}")];
        if name == "RxDma" {
            base += model.constants["src/platform.rs:RX_DMA_OFFSET"];
        }
        for f in &block.fields {
            let row = |layout: &str, field: &str| {
                format!("{page}.html#{}-{}", anchor(layout), field.replace('_', "-"))
            };
            let Type::Array(a) = &f.ty else {
                registers.push((
                    base + f.offset,
                    f.name.to_uppercase(),
                    title.to_owned(),
                    row(name, &f.name),
                ));
                continue;
            };
            let count = number(&a.len).context("array length")?;
            let stride = model.shape(&a.elem)?.size;
            // An array of a layout, such as the HIF descriptors: one row per
            // word of every element, linked to the element's own field.
            let inner = match &*a.elem {
                Type::Path(p) => p
                    .path
                    .get_ident()
                    .and_then(|i| model.layouts.get(&i.to_string()))
                    .map(|(b, _)| b),
                _ => None,
            };
            for i in 0..count {
                let at = base + f.offset + i * stride;
                let label = format!("{}[{i}]", f.name.to_uppercase());
                match inner {
                    Some(element) => {
                        for e in &element.fields {
                            registers.push((
                                at + e.offset,
                                format!("{label}.{}", e.name.to_uppercase()),
                                title.to_owned(),
                                row(&element.name, &e.name),
                            ));
                        }
                    }
                    None => registers.push((at, label, title.to_owned(), row(name, &f.name))),
                }
            }
        }
    }
    for (offset, field, row) in &download {
        registers.push((
            DOWNLOAD_CONTROL + offset,
            field.clone(),
            "Download control".into(),
            format!("boot.html#{row}"),
        ));
    }
    for pipe in 0..4 {
        for (offset, field, row) in &ring {
            registers.push((
                ring_base + pipe * 0x80 + offset,
                format!("{field} (pipe {pipe})"),
                "Transmit rings".into(),
                format!("rings.html#{row}"),
            ));
        }
    }
    let mac_base = model.constants["src/platform.rs:MAC_REGISTER_BASE"];
    for (file, base, block, page) in [
        ("mac.yml", mac_base, "MAC", "mac"),
        ("phy.yml", 0, "PHY and RF", "phy"),
        ("platform.yml", 0, "Platform control", "platform"),
    ] {
        let rows = values::hand_rows(
            &fs::read_to_string(root.join("docs/_data").join(file))?,
            base,
        )
        .with_context(|| format!("reading {file}"))?;
        for r in rows {
            let stem = r.name.split('[').next().unwrap_or(&r.name).to_owned();
            let url = format!(
                "{}.html#{page}-{}",
                r.page.as_deref().unwrap_or(page),
                stem.to_lowercase().replace('_', "-")
            );
            for i in 0..r.count {
                let label = if r.count > 1 {
                    format!("{stem}[{i}]")
                } else {
                    r.name.clone()
                };
                registers.push((
                    r.address + i * r.stride,
                    label,
                    r.block.clone().unwrap_or_else(|| block.into()),
                    url.clone(),
                ));
            }
        }
    }
    let mut unnamed = Vec::new();
    for (i, (address, value)) in image_writes.iter().enumerate() {
        unnamed.push((
            *address,
            format!("receive image #{i}: `{}`", hex32(*value)),
            "MAC".to_owned(),
            format!("mac.html#rx-image-{i}"),
        ));
    }
    for (i, (address, value)) in loader_writes.iter().enumerate() {
        unnamed.push((
            *address,
            format!("loader write #{i}: `{}`", hex32(*value)),
            "PHY and RF".to_owned(),
            format!("phy.html#loader-{i}"),
        ));
    }
    let named: std::collections::BTreeSet<u32> = registers.iter().map(|r| r.0).collect();
    registers.extend(unnamed.into_iter().filter(|r| !named.contains(&r.0)));
    registers.sort();
    registers.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut json = String::from("[\n");
    for (i, (address, name, block, url)) in registers.iter().enumerate() {
        writeln!(
            json,
            "  {{\"address\": \"{}\", \"name\": \"{}\", \"block\": \"{block}\", \"url\": \"{url}\"}}{}",
            hex32(*address),
            name.replace('"', "\\\""),
            if i + 1 < registers.len() { "," } else { "" }
        )?;
    }
    json.push_str("]\n");
    fs::write(root.join("docs/_data/registers.json"), json)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arrays_gaps_and_conditional_macros() {
        let mut m = Model::default();
        m.add("source.rs","#[cfg(target_arch=\"arm\")] register_structs!{D{(0=>x:ReadOnly<u16>),(2=>_reserved),(8=>@END)}, T{(0=>a:[D;3]),(24=>v:ReadWrite<u32>),(28=>@END)}}").unwrap();
        let t = m.table("T", Some(0x1000)).unwrap();
        assert!(t.contains("3 × [D](#d)"));
        assert!(t.contains("0x0000_1018"));
        assert!(m.table("D", None).unwrap().contains("2 B"));
    }
    #[test]
    fn unknown_fields_fail() {
        let mut m = Model::default();
        m.add("s", "register_structs!{D{(0=>x:Unknown),(4=>@END)}}")
            .unwrap();
        assert!(m.table("D", None).is_err());
    }
    #[test]
    fn documented_fields_link_their_bits() {
        let mut m = Model::default();
        m.add(
            "s",
            "register_bitfields![u32, Ctl [ /// Go.\n ON OFFSET(0) NUMBITS(1) [] ]];
             register_structs!{D{ /// Start here.\n (0=>c:ReadWrite<u32, Ctl::Register>),(4=>@END)}}",
        )
        .unwrap();
        let t = m.table("D", Some(0)).unwrap();
        assert!(t.contains("[`C`](#ctl-bits)"), "{t}");
        assert!(t.contains("<span id=\"d-c\"></span>"), "{t}");
        assert!(t.contains("| Start here. |"), "{t}");
        let mut pipes = Model::default();
        pipes
            .add(
                "s",
                "register_structs!{D{ /// `a | b` or c|d\n (0=>c:ReadWrite<u32>),(4=>@END)}}",
            )
            .unwrap();
        let t = pipes.table("D", Some(0)).unwrap();
        assert!(t.contains("<code>a &#124; b</code> or c\\|d"), "{t}");
        assert!(m.bits["Ctl"].contains("Go."));
    }
    #[test]
    fn hand_rows_parse_offsets_ranges_and_quotes() {
        let rows = values::hand_rows(
            "- section: x\n  rows:\n    - {offset: \"0x0e70\", name: \"Q[0..3]\", count: 4, use: W, text: \"a, b: c\"}\n    - {address: \"0x0abb_8004\", name: CAL, page: other}",
            0x09c0_0000,
        )
        .unwrap();
        assert_eq!(rows[0].address, 0x09c0_0e70);
        assert_eq!((rows[0].count, rows[0].stride), (4, 4));
        assert_eq!(rows[1].address, 0x0abb_8004);
        assert_eq!(rows[1].page.as_deref(), Some("other"));
    }
    #[test]
    fn anchors_are_kebab_case() {
        assert_eq!(anchor("RxDmaControl"), "rx-dma-control");
        assert_eq!(hex32(0x0ac0_001c), "0x0ac0_001c");
        assert_eq!(hex_grouped(0x6_0000), "0x6_0000");
        assert_eq!(hex_grouped(0xc0), "0xc0");
    }
}
