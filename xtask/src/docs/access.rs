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
        String::from("| Offset | CPU address | Function in `src/mac.rs` |\n|---:|---:|---|\n");
    for (off, names) in c.rows {
        let a = base.checked_add(off).context("MAC address overflow")?;
        let names = names
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(md, "| `+0x{off:04x}` | `0x{a:08x}` | {names} |")?;
    }
    Ok(md)
}
/// Only plain integer words and byte arrays occur in this repr(C) hardware layout.
fn c_shape(ty: &Type) -> Result<(u32, u32)> {
    match ty {
        Type::Path(p) if p.path.is_ident("u32") => Ok((4, 4)),
        Type::Path(p) if p.path.is_ident("u8") => Ok((1, 1)),
        Type::Array(a) => {
            let (n, align) = c_shape(&a.elem)?;
            Ok((
                n.checked_mul(number(&a.len).context("nonliteral C array")?)
                    .context("array overflow")?,
                align,
            ))
        }
        _ => bail!("unsupported C field"),
    }
}
pub(super) fn c_layout(source: &str, name: &str) -> Result<String> {
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
    let mut md = String::from("| Offset | Field | Size |\n|---:|---|---:|\n");
    let mut offset = 0u32;
    for f in &s.fields {
        let (size, align) = c_shape(&f.ty)?;
        offset = offset
            .checked_next_multiple_of(align)
            .context("layout overflow")?;
        writeln!(
            md,
            "| `+0x{offset:02x}` | `{}` | {size} B |",
            f.ident.as_ref().context("unnamed field")?
        )?;
        offset = offset.checked_add(size).context("layout overflow")?;
    }
    Ok(md)
}
