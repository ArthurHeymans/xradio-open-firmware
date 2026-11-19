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
struct Field {
    offset: u32,
    name: String,
    ty: Type,
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
            rows.call(Attribute::parse_outer)?;
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
fn number(e: &Expr) -> Option<u32> {
    if let Expr::Lit(l) = e
        && let Lit::Int(i) = &l.lit
    {
        i.base10_parse().ok()
    } else {
        None
    }
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
                Item::Macro(m) if m.mac.path.is_ident("register_structs") => {
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
                Item::Macro(m) if m.mac.path.is_ident("register_bitfields") => {
                    for group in m.mac.parse_body::<values::BitGroups>()?.0 {
                        ensure!(
                            self.bits.insert(group.name, group.markdown).is_none(),
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
    fn shape(&self, ty: &Type) -> Result<(String, u32)> {
        match ty {
            Type::Array(a) => {
                let count = number(&a.len).context("expected literal array length")?;
                let (element, stride) = self.shape(&a.elem)?;
                Ok((
                    format!("{count} × {element} (stride {stride} B)"),
                    count.checked_mul(stride).context("array size overflow")?,
                ))
            }
            Type::Path(p) => {
                let seg = p.path.segments.last().context("empty field type")?;
                let name = seg.ident.to_string();
                if let Some((block, _)) = self.layouts.get(&name) {
                    return Ok((name, block.size));
                }
                ensure!(
                    ["ReadWrite", "ReadOnly", "WriteOnly"].contains(&name.as_str()),
                    "unknown register type {name}"
                );
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments
                    && let Some(syn::GenericArgument::Type(Type::Path(t))) = args.args.first()
                    && let Some(id) = t.path.get_ident()
                {
                    let width = match id.to_string().as_str() {
                        "u32" => 4,
                        "u16" => 2,
                        "u8" => 1,
                        _ => bail!("unsupported register width {id}"),
                    };
                    return Ok((format!("{id} · {name}"), width));
                }
                bail!("missing register width")
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
            "[`{name}`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/{path}) · {} bytes\n\n| Offset | {} | Field | Firmware type |\n|---:|---:|---|---|\n",
            block.size,
            if base.is_some() {
                "CPU address"
            } else {
                "Size"
            }
        );
        let mut last = 0;
        for f in &block.fields {
            let (shape, size) = self.shape(&f.ty)?;
            ensure!(size > 0 && f.offset >= last, "overlapping fields in {name}");
            last = f.offset.checked_add(size).context("field overflow")?;
            ensure!(last <= block.size, "field exceeds {name}");
            let address = if let Some(b) = base {
                format!(
                    "`0x{:08x}`",
                    b.checked_add(f.offset).context("address overflow")?
                )
            } else {
                format!("{size} B")
            };
            writeln!(
                out,
                "| `+0x{:02x}` | {address} | `{}` | {shape} |",
                f.offset, f.name
            )?;
        }
        Ok(out)
    }
}
const PLAT: &str = "src/platform.rs";
const HIF: &str = "src/hif.rs";
const IRQ: &str = "src/interrupts.rs";
// Join layout names to address *symbols*, not copies of hardware addresses.
const BLOCKS: &[(&str, &str, &str, &str)] = &[
    ("AesRegisters", PLAT, "AES_REGISTER_BASE", "aes"),
    ("RxShared", HIF, "RX_DESCRIPTOR_BASE", "hif"),
    ("HifShared", HIF, "TX_DESCRIPTOR_BASE", "hif"),
    ("InterruptController", IRQ, "BASE", "interrupts"),
    ("SystemControl", PLAT, "SYSTEM_CONTROL_BASE", "system"),
    ("ClockReset", PLAT, "CLOCK_RESET_BASE", "clock"),
    ("Timers", PLAT, "TIMERS_BASE", "timers"),
    ("PlatformControl", PLAT, "PLATFORM_CONTROL_BASE", "platform"),
    ("RxDma", PLAT, "MAC_REGISTER_BASE", "rx-dma"),
];
pub fn generate(root: &Path) -> Result<()> {
    let mut model = Model::default();
    for path in [PLAT, HIF, IRQ, "src/crypto/engine.rs", "src/radio.rs"] {
        model
            .add(path, &fs::read_to_string(root.join(path))?)
            .with_context(|| format!("reading {path}"))?;
    }
    let dir = root.join("docs/_includes/registers");
    fs::create_dir_all(&dir)?;
    let mut index = values::windows(&fs::read_to_string(root.join("src/mmio.rs"))?)?;
    for &(name, file, symbol, page) in BLOCKS {
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
        index.push((addr, name.to_owned(), page.to_owned()));
    }
    let relative = ["Descriptor", "InterruptRouting", "SlotHeader"];
    for name in relative {
        fs::write(dir.join(format!("{name}.md")), model.table(name, None)?)?;
    }
    ensure!(
        BLOCKS.len() + relative.len() == model.layouts.len(),
        "new register block needs an address or relative layout"
    );
    for (name, table) in model.bits {
        fs::write(dir.join(format!("{name}.md")), table)?;
    }
    fs::write(
        dir.join("LoaderWrites.md"),
        values::loader(&fs::read_to_string(root.join("src/loader.rs"))?)?,
    )?;
    index.sort();
    let mut out = String::from("| CPU base | Firmware block |\n|---:|---|\n");
    for (addr, name, page) in index {
        writeln!(out, "| `0x{addr:08x}` | [{name}]({page}.html) |")?;
    }
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
    fs::write(
        dir.join("TxRing.md"),
        access::c_layout(
            &fs::read_to_string(root.join("src/tx/pipe.rs"))?,
            "TxHardwareRingLayout",
        )?,
    )?;
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
        assert!(t.contains("3 × D (stride 8 B)"));
        assert!(t.contains("0x00001018"));
        assert!(m.table("D", None).unwrap().contains("2 B"));
    }
    #[test]
    fn unknown_fields_fail() {
        let mut m = Model::default();
        m.add("s", "register_structs!{D{(0=>x:Unknown),(4=>@END)}}")
            .unwrap();
        assert!(m.table("D", None).is_err());
    }
}
