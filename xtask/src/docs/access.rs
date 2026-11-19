//! Inventory literal register selections, not a disassembly or a claim about silicon semantics.
use super::*;
use std::collections::BTreeSet;
use syn::visit::{self, Visit};
#[derive(Default)]
struct Calls {
    owner: String,
    rows: BTreeMap<u32, BTreeSet<String>>,
}
impl<'ast> Visit<'ast> for Calls {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let prev = std::mem::replace(&mut self.owner, f.sig.ident.to_string());
        visit::visit_item_fn(self, f);
        self.owner = prev;
    }
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*c.func
            && p.path
                .segments
                .last()
                .is_some_and(|s| s.ident == "mac_register")
            && let Some(off) = c.args.first().and_then(number)
        {
            self.rows.entry(off).or_default().insert(self.owner.clone());
        }
        visit::visit_expr_call(self, c);
    }
}
pub(super) fn mac(source: &str, base: u32) -> Result<String> {
    let mut c = Calls::default();
    c.visit_file(&syn::parse_file(source)?);
    ensure!(!c.rows.is_empty(), "no MAC register selections");
    let mut md =
        String::from("| Offset | Address | Functions in `src/mac.rs` |\n|---:|---:|---|\n");
    for (off, names) in c.rows {
        let a = base.checked_add(off).context("MAC address overflow")?;
        let names = names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(md, "| `+0x{off:04x}` | `{}` | {names} |", hex32(a))?;
    }
    md.push_str("{: .registers}\n");
    Ok(md)
}
/// Only plain integer words and byte arrays occur in this repr(C) hardware layout.
fn c_shape(ty: &Type) -> Result<(u32, u32, String)> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        let access = match seg.ident.to_string().as_str() {
            "ReadWrite" => Some("RW"),
            "ReadOnly" => Some("RO"),
            "WriteOnly" => Some("WO"),
            _ => None,
        };
        if let Some(access) = access {
            return Ok((4, 4, format!("u32 {access}")));
        }
    }
    match ty {
        Type::Path(p) if p.path.is_ident("u32") => Ok((4, 4, "u32".into())),
        Type::Path(p) if p.path.is_ident("u8") => Ok((1, 1, "u8".into())),
        Type::Array(a) => {
            let (n, align, element) = c_shape(&a.elem)?;
            let count = number(&a.len).context("nonliteral C array")?;
            Ok((
                n.checked_mul(count).context("array overflow")?,
                align,
                format!("{count} × {element}"),
            ))
        }
        _ => bail!("unsupported C field"),
    }
}
/// One field of a C layout: offset, register name and row anchor.
pub(super) type CField = (u32, String, String);
/// A `repr(C)` layout as a register table; `base` adds an address column.
/// Returns the table and every field.
pub(super) fn c_layout(
    path: &str,
    source: &str,
    name: &str,
    base: Option<u32>,
) -> Result<(String, Vec<CField>)> {
    let file = syn::parse_file(source)?;
    let s = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Struct(s) if s.ident == name => Some(s),
            _ => None,
        })
        .context("missing C layout")?;
    ensure!(
        s.attrs
            .iter()
            .any(|a| a.path().is_ident("repr") && a.parse_args::<Ident>().is_ok_and(|i| i == "C")),
        "expected repr(C)"
    );
    let mut md = format!(
        "| Offset | {} | Register | Type | Description |\n|---:|---:|---|---|---|\n",
        if base.is_some() { "Address" } else { "Size" }
    );
    let mut rows = Vec::new();
    let mut offset = 0u32;
    for f in &s.fields {
        let (size, align, ty) = c_shape(&f.ty)?;
        offset = offset
            .checked_next_multiple_of(align)
            .context("layout overflow")?;
        let field = f.ident.as_ref().context("unnamed field")?.to_string();
        if field.starts_with('_') {
            offset = offset.checked_add(size).context("layout overflow")?;
            continue;
        }
        let row = format!("{}-{}", anchor(name), field.replace('_', "-"));
        let column = match base {
            Some(b) => format!("`{}`", hex32(b.checked_add(offset).context("overflow")?)),
            None => format!("{size} B"),
        };
        writeln!(
            md,
            "| `+0x{offset:02x}` | {column} | <span id=\"{row}\"></span>`{}` | {ty} | {} |",
            field.to_uppercase(),
            doc(&f.attrs)
        )?;
        rows.push((offset, field.to_uppercase(), row));
        offset = offset.checked_add(size).context("layout overflow")?;
    }
    writeln!(
        md,
        "{{: .registers}}\n\n<p class=\"source\">Source: <a href=\"{REPOSITORY}/{path}\"><code>{}::{name}</code></a> · {offset} bytes</p>",
        path.trim_start_matches("src/")
            .trim_end_matches(".rs")
            .replace('/', "::")
    )?;
    Ok((md, rows))
}
