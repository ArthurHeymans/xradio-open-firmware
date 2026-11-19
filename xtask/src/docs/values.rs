use super::*;
use syn::{ExprArray, bracketed};

pub(super) struct Bits {
    pub name: String,
    pub markdown: String,
}
pub(super) struct BitGroups(pub Vec<Bits>);
impl Parse for BitGroups {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let word = input.parse::<Ident>()?;
        if word != "u32" {
            return Err(input.error("unsupported bitfield word"));
        }
        input.parse::<Token![,]>()?;
        let mut groups = Vec::new();
        while !input.is_empty() {
            input.call(Attribute::parse_outer)?;
            input.parse::<Visibility>()?;
            let name = input.parse::<Ident>()?.to_string();
            let members;
            bracketed!(members in input);
            let mut md = String::from("| Field | Bits |\n|---|---:|\n");
            while !members.is_empty() {
                members.call(Attribute::parse_outer)?;
                let field = members.parse::<Ident>()?;
                let literal = |label: &str| -> syn::Result<u32> {
                    if members.parse::<Ident>()? != label {
                        return Err(members.error("unexpected bitfield expression"));
                    }
                    let n;
                    parenthesized!(n in members);
                    let v = n.parse::<LitInt>()?.base10_parse()?;
                    if !n.is_empty() {
                        return Err(n.error("expected literal"));
                    }
                    Ok(v)
                };
                let start = literal("OFFSET")?;
                let len = literal("NUMBITS")?;
                let end = start
                    .checked_add(len)
                    .filter(|&e| len > 0 && e <= 32)
                    .ok_or_else(|| members.error("invalid field width"))?
                    - 1;
                let enumerants;
                bracketed!(enumerants in members);
                if !enumerants.is_empty() {
                    return Err(enumerants.error("named enumerants not supported"));
                }
                let bits = if start == end {
                    start.to_string()
                } else {
                    format!("{end}:{start}")
                };
                writeln!(md, "| `{field}` | {bits} |").unwrap();
                if !members.is_empty() {
                    members.parse::<Token![,]>()?;
                }
            }
            groups.push(Bits { name, markdown: md });
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self(groups))
    }
}
pub(super) fn loader(source: &str) -> Result<String> {
    let file = syn::parse_file(source)?;
    let c = file
        .items
        .into_iter()
        .find_map(|i| match i {
            Item::Const(c) if c.ident == "VENDOR_REGISTER_INITIALIZATION" => Some(c),
            _ => None,
        })
        .context("missing loader sequence")?;
    let Expr::Reference(r) = *c.expr else {
        bail!("expected loader slice")
    };
    let Expr::Array(ExprArray { elems, .. }) = *r.expr else {
        bail!("expected loader array")
    };
    let mut md = String::from("| Write | Address | Value |\n|---:|---:|---:|\n");
    for (i, e) in elems.iter().enumerate() {
        let Expr::Tuple(t) = e else {
            bail!("expected address/value pair")
        };
        ensure!(t.elems.len() == 2, "invalid loader write");
        let a = number(&t.elems[0]).context("invalid loader address")?;
        let v = number(&t.elems[1]).context("invalid loader value")?;
        writeln!(md, "| {i} | `0x{a:08x}` | `0x{v:08x}` |")?;
    }
    Ok(md)
}
pub(super) fn messages(source: &str) -> Result<String> {
    let mut entries = Vec::new();
    for item in syn::parse_file(source)?.items {
        let Item::Const(c) = item else { continue };
        let name = c.ident.to_string();
        if !name.ends_with("_ID") {
            continue;
        }
        let id = number(&c.expr).context("expected literal WSM message ID")?;
        let direction = if name.ends_with("_REQ_ID") {
            "host → firmware"
        } else {
            "firmware → host"
        };
        entries.push((id, name, direction));
    }
    ensure!(!entries.is_empty(), "missing WSM message IDs");
    entries.sort();
    let mut md = String::from("| ID | Direction | Message constant |\n|---:|---|---|\n");
    for (id, name, direction) in entries {
        writeln!(md, "| `0x{id:04x}` | {direction} | `{name}` |")?;
    }
    Ok(md)
}
pub(super) fn windows(source: &str) -> Result<Vec<(u32, String, String)>> {
    let mut rows = Vec::new();
    for i in syn::parse_file(source)?.items {
        let Item::Static(s) = i else { continue };
        let name = s.ident.to_string();
        let page = match name.as_str() {
            "MAC" => "mac",
            "TX_RINGS" => "rings",
            "PHY" => "phy",
            "PLATFORM_CONTROL" => "platform",
            _ => bail!("unmapped MMIO window {name}"),
        };
        let Expr::Unsafe(u) = *s.expr else {
            bail!("expected window initializer")
        };
        let [syn::Stmt::Expr(Expr::Call(c), None)] = u.block.stmts.as_slice() else {
            bail!("expected window constructor")
        };
        ensure!(
            matches!(&*c.func, Expr::Path(p) if p.path.segments.last().is_some_and(|s|s.ident=="new"))
                && c.args.len() == 2,
            "invalid window"
        );
        let base = number(&c.args[0]).context("invalid base")?;
        let size = number(&c.args[1]).context("invalid window size")?;
        rows.push((
            base,
            format!("{name} aperture ({size:#x} bytes)"),
            page.into(),
        ));
    }
    Ok(rows)
}
