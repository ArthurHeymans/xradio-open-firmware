use super::*;
use syn::{ExprArray, bracketed};

pub(super) struct BitField {
    name: String,
    start: u32,
    len: u32,
    doc: String,
    values: Vec<(String, u32, String)>,
}
pub(super) struct Bits {
    pub name: String,
    doc: String,
    fields: Vec<BitField>,
}
impl Bits {
    /// A bit diagram in rows of eight (rows without a described bit are
    /// left out), followed by the field table.
    pub(super) fn markdown(&self, path: &str) -> String {
        let mut owner = [None::<usize>; 32];
        for (i, f) in self.fields.iter().enumerate() {
            for bit in f.start..f.start + f.len {
                owner[bit as usize] = Some(i);
            }
        }
        let id = format!("{}-bits", anchor(&self.name));
        let mut out = format!(
            "<div class=\"bitfield\" id=\"{id}\"><div class=\"bf-grid\" aria-hidden=\"true\">"
        );
        let mut drawn = [false; 32];
        for lo in [24u32, 16, 8, 0] {
            let hi = lo + 7;
            // Keep bit columns aligned: 31:24 and 15:8 on the left, 23:16
            // and 7:0 on the right, upper half-word above the lower.
            let place = format!(
                "{} {}",
                if lo % 16 == 8 { "bf-c1" } else { "bf-c2" },
                if lo >= 16 { "bf-hi" } else { "bf-lo" }
            );
            if (lo..=hi).all(|b| owner[b as usize].is_none()) {
                write!(
                    out,
                    "<div class=\"bf-row bf-skip {place}\"><span class=\"bf-gap\">{hi}:{lo} not described</span></div>"
                )
                .unwrap();
                continue;
            }
            write!(out, "<div class=\"bf-row {place}\">").unwrap();
            for bit in (lo..=hi).rev() {
                write!(out, "<span class=\"bf-n\">{bit}</span>").unwrap();
            }
            let mut bit = hi as i32;
            while bit >= lo as i32 {
                let who = owner[bit as usize];
                let mut end = bit;
                while end > lo as i32 && owner[(end - 1) as usize] == who {
                    end -= 1;
                }
                let span = (bit - end + 1) as usize;
                match who {
                    Some(i) if drawn[i] => {
                        // A field's second row: a lighter continuation.
                        let name = &self.fields[i].name;
                        write!(
                            out,
                            "<span class=\"bf-f bf-cont\" style=\"grid-column:span {span}\" title=\"{name}\">{name} …</span>"
                        )
                        .unwrap();
                    }
                    Some(i) => {
                        drawn[i] = true;
                        let name = &self.fields[i].name;
                        // About six label characters fit across one bit.
                        let class = if name.len() > span * 6 {
                            "bf-f bf-v"
                        } else {
                            "bf-f"
                        };
                        write!(
                            out,
                            "<span class=\"{class}\" style=\"grid-column:span {span}\" title=\"{name}\">{name}</span>"
                        )
                        .unwrap();
                    }
                    None => write!(
                        out,
                        "<span class=\"bf-r\" style=\"grid-column:span {span}\"></span>"
                    )
                    .unwrap(),
                }
                bit = end - 1;
            }
            out.push_str("</div>");
        }
        out.push_str("</div></div>\n\n");
        if !self.doc.is_empty() {
            writeln!(out, "{}\n", self.doc).unwrap();
        }
        out.push_str("| Bits | Field | Description |\n|---:|---|---|\n");
        let mut fields: Vec<_> = self.fields.iter().collect();
        fields.sort_by_key(|f| std::cmp::Reverse(f.start));
        for f in fields {
            let end = f.start + f.len - 1;
            let bits = if f.len == 1 {
                f.start.to_string()
            } else {
                format!("{end}:{}", f.start)
            };
            let mut text = f.doc.clone();
            for (name, value, doc) in &f.values {
                write!(text, "<br><code>{value}</code> <code>{name}</code>").unwrap();
                if !doc.is_empty() {
                    write!(text, ": {doc}").unwrap();
                }
            }
            writeln!(out, "| {bits} | <code>{}</code> | {text} |", f.name).unwrap();
        }
        writeln!(
            out,
            "{{: .bits}}\n\n<p class=\"source\">Source: <a href=\"{REPOSITORY}/{path}\"><code>{}</code></a> · 4 bytes</p>",
            self.name
        )
        .unwrap();
        out
    }
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
            let group_doc = doc(&input.call(Attribute::parse_outer)?);
            input.parse::<Visibility>()?;
            let name = input.parse::<Ident>()?.to_string();
            let members;
            bracketed!(members in input);
            let mut fields = Vec::new();
            let mut used = 0u32;
            while !members.is_empty() {
                let field_doc = doc(&members.call(Attribute::parse_outer)?);
                let field = members.parse::<Ident>()?.to_string();
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
                if len == 0 || start.checked_add(len).is_none_or(|e| e > 32) {
                    return Err(members.error("invalid field width"));
                }
                let mask = (u32::MAX >> (32 - len)) << start;
                if used & mask != 0 {
                    return Err(members.error("overlapping bitfields"));
                }
                used |= mask;
                let enumerants;
                bracketed!(enumerants in members);
                let mut values = Vec::new();
                while !enumerants.is_empty() {
                    let value_doc = doc(&enumerants.call(Attribute::parse_outer)?);
                    let value_name = enumerants.parse::<Ident>()?.to_string();
                    enumerants.parse::<Token![=]>()?;
                    let value = enumerants.parse::<LitInt>()?.base10_parse()?;
                    values.push((value_name, value, value_doc));
                    if !enumerants.is_empty() {
                        enumerants.parse::<Token![,]>()?;
                    }
                }
                fields.push(BitField {
                    name: field,
                    start,
                    len,
                    doc: field_doc,
                    values,
                });
                if !members.is_empty() {
                    members.parse::<Token![,]>()?;
                }
            }
            groups.push(Bits {
                name,
                doc: group_doc,
                fields,
            });
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self(groups))
    }
}
pub(super) fn loader(source: &str) -> Result<(String, Vec<(u32, u32)>)> {
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
    let mut addresses = Vec::new();
    for (i, e) in elems.iter().enumerate() {
        let Expr::Tuple(t) = e else {
            bail!("expected address/value pair")
        };
        ensure!(t.elems.len() == 2, "invalid loader write");
        let a = number(&t.elems[0]).context("invalid loader address")?;
        let v = number(&t.elems[1]).context("invalid loader value")?;
        writeln!(
            md,
            "| <span id=\"loader-{i}\"></span>{i} | `{}` | `{}` |",
            hex32(a),
            hex32(v)
        )?;
        addresses.push((a, v));
    }
    md.push_str("{: .registers .compact}\n");
    Ok((md, addresses))
}
pub(super) fn messages(source: &str) -> Result<String> {
    let mut ids = BTreeMap::new();
    for item in syn::parse_file(source)?.items {
        let Item::Const(c) = item else { continue };
        let name = c.ident.to_string();
        if !name.ends_with("_ID") {
            continue;
        }
        let id = number(&c.expr).context("expected literal WSM message ID")?;
        ensure!(
            ids.insert(id, (name, doc(&c.attrs))).is_none(),
            "duplicate WSM message ID"
        );
    }
    ensure!(!ids.is_empty(), "missing WSM message IDs");
    let message = |name: &str| name.strip_suffix("_ID").unwrap_or(name).to_owned();
    // A confirmation carries its request's ID with bit 10 set; requests
    // without a confirmation constant get a generic one.
    let mut md = String::from("| Request | Confirmation | Purpose |\n|---|---|---|\n");
    for (&id, (name, doc)) in &ids {
        if name.ends_with("_REQ_ID") {
            let confirm = id | 0x0400;
            let reply = match ids.get(&confirm) {
                Some((c, _)) => format!("`0x{confirm:04x}` `{}`", message(c)),
                None => format!("`0x{confirm:04x}` generic"),
            };
            writeln!(md, "| `0x{id:04x}` `{}` | {reply} | {doc} |", message(name))?;
        }
    }
    for (&id, (name, doc)) in &ids {
        if id & 0x0c00 == 0x0400 && !ids.contains_key(&(id & !0x0400)) {
            writeln!(md, "| — | `0x{id:04x}` `{}` | {doc} |", message(name))?;
        }
    }
    md.push_str("{: .messages}\n\nA generic confirmation carries the request's ID with bit 10 set and only a status word.\n{: .source}\n\n| Indication | Purpose |\n|---|---|\n");
    for (&id, (name, doc)) in &ids {
        if id & 0x0c00 == 0x0800 {
            writeln!(md, "| `0x{id:04x}` `{}` | {doc} |", message(name))?;
        }
    }
    md.push_str("{: .messages}\n");
    Ok(md)
}
pub(super) struct Window {
    pub base: u32,
    pub size: u32,
    pub title: String,
    pub page: String,
}
pub(super) fn windows(source: &str) -> Result<Vec<Window>> {
    let mut rows = Vec::new();
    for i in syn::parse_file(source)?.items {
        let Item::Static(s) = i else { continue };
        let name = s.ident.to_string();
        let (page, title) = match name.as_str() {
            "MAC" => ("mac", "MAC and packet DMA"),
            "TX_RINGS" => ("rings", "Transmit rings"),
            "PHY" => ("phy", "PHY and RF"),
            "PLATFORM_CONTROL" => ("platform", "Platform control"),
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
        rows.push(Window {
            base,
            size,
            title: title.into(),
            page: page.into(),
        });
    }
    Ok(rows)
}

/// The fixed register image written by one function: `let` base literals,
/// `for (offset, value) in [...]` loops over `register32(base + offset)`,
/// and direct `register32(address).set(value)` writes, in write order.
pub(super) fn register_image(source: &str, function: &str) -> Result<(String, Vec<(u32, u32)>)> {
    let file = syn::parse_file(source)?;
    let f = file
        .items
        .iter()
        .find_map(|i| match i {
            Item::Fn(f) if f.sig.ident == function => Some(f),
            _ => None,
        })
        .context("missing register image function")?;
    let mut locals = BTreeMap::new();
    let mut writes = Vec::new();
    // `register32(<base expr>)` -> base address, where the expression is a
    // literal, a local, or one of those plus the loop's `offset`.
    let base = |e: &Expr, locals: &BTreeMap<String, u32>| -> Option<u32> {
        let leaf = |e: &Expr| {
            number(e).or_else(|| match e {
                Expr::Path(p) => p
                    .path
                    .get_ident()
                    .and_then(|i| locals.get(&i.to_string()).copied()),
                _ => None,
            })
        };
        match e {
            Expr::Binary(b) if matches!(b.op, syn::BinOp::Add(_)) => leaf(&b.left),
            e => leaf(e),
        }
    };
    let target = |e: &Expr, locals: &BTreeMap<String, u32>| -> Option<(u32, Expr)> {
        let Expr::MethodCall(m) = e else { return None };
        if m.method != "set" || m.args.len() != 1 {
            return None;
        }
        let Expr::Call(c) = &*m.receiver else {
            return None;
        };
        if !matches!(&*c.func, Expr::Path(p) if p.path.is_ident("register32")) {
            return None;
        }
        Some((base(c.args.first()?, locals)?, m.args[0].clone()))
    };
    for stmt in &f.block.stmts {
        match stmt {
            syn::Stmt::Local(l) => {
                if let syn::Pat::Ident(i) = &l.pat
                    && let Some(init) = &l.init
                    && let Some(v) = number(&init.expr)
                {
                    locals.insert(i.ident.to_string(), v);
                }
            }
            syn::Stmt::Expr(Expr::ForLoop(fl), _) => {
                let Expr::Array(a) = &*fl.expr else {
                    bail!("expected an array of writes")
                };
                let [syn::Stmt::Expr(body, _)] = fl.body.stmts.as_slice() else {
                    bail!("expected one write per iteration")
                };
                let (b, _) = target(body, &locals).context("unrecognized image write")?;
                for e in &a.elems {
                    let Expr::Tuple(t) = e else {
                        bail!("expected (offset, value)")
                    };
                    let off = number(&t.elems[0]).context("offset")?;
                    let v = number(&t.elems[1]).context("value")?;
                    writes.push((b.checked_add(off).context("overflow")?, v));
                }
            }
            syn::Stmt::Expr(e, _) => {
                let (a, v) = target(e, &locals).context("unrecognized image write")?;
                writes.push((a, number(&v).context("value")?));
            }
            _ => bail!("unsupported statement in register image"),
        }
    }
    ensure!(!writes.is_empty(), "empty register image");
    let mut md = String::from("| # | Address | Value |\n|---:|---:|---:|\n");
    for (i, (a, v)) in writes.iter().enumerate() {
        writeln!(
            md,
            "| <span id=\"rx-image-{i}\"></span>{i} | `{}` | `{}` |",
            hex32(*a),
            hex32(*v)
        )?;
    }
    md.push_str("{: .registers .compact}\n");
    Ok((md, writes.iter().map(|&(a, v)| (a, v)).collect()))
}

/// One row of a hand-maintained register data file (`docs/_data/*.yml`).
pub(super) struct HandRow {
    pub address: u32,
    pub name: String,
    pub count: u32,
    pub stride: u32,
    /// The page that shows the row, when not the data file's own page.
    pub page: Option<String>,
    /// The block named in the index, when not the data file's own.
    pub block: Option<String>,
}
/// Parse the restricted YAML of the register data files: one flow mapping
/// per row, `- {offset: "0x0030", name: OWN_MAC_LOW, ...}`. `offset` is
/// relative to `base`; `address` is absolute.
pub(super) fn hand_rows(source: &str, base: u32) -> Result<Vec<HandRow>> {
    let mut rows = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(body) = line.strip_prefix("- {").and_then(|l| l.strip_suffix('}')) else {
            continue;
        };
        let mut fields = BTreeMap::new();
        let mut rest = body;
        while !rest.is_empty() {
            let (key, after) = rest.split_once(':').context("expected key")?;
            let after = after.trim_start();
            let (value, tail) = if let Some(quoted) = after.strip_prefix('"') {
                let end = quoted.find('"').context("unterminated string")?;
                (&quoted[..end], &quoted[end + 1..])
            } else {
                match after.find(',') {
                    Some(end) => (&after[..end], &after[end..]),
                    None => (after, ""),
                }
            };
            fields.insert(key.trim().to_owned(), value.trim().to_owned());
            rest = tail.trim_start().trim_start_matches(',').trim_start();
        }
        let hex = |v: &str| u32::from_str_radix(&v.replace('_', "")[2..], 16);
        let address = match (fields.get("offset"), fields.get("address")) {
            (Some(o), None) => base + hex(o)?,
            (None, Some(a)) => hex(a)?,
            _ => bail!("row needs exactly one of offset and address: {line}"),
        };
        let number = |k: &str, default: u32| {
            fields
                .get(k)
                .map_or(Ok(default), |v| v.parse::<u32>().context("bad number"))
        };
        rows.push(HandRow {
            address,
            name: fields.get("name").context("row without name")?.clone(),
            count: number("count", 1)?,
            stride: number("stride", 4)?,
            page: fields.get("page").cloned(),
            block: fields.get("block").cloned(),
        });
    }
    ensure!(!rows.is_empty(), "no register rows");
    Ok(rows)
}

/// MIB IDs and their purposes from the `MIB_ID_` constants.
pub(super) fn mibs(source: &str) -> Result<String> {
    let mut rows = BTreeMap::new();
    for item in syn::parse_file(source)?.items {
        let Item::Const(c) = item else { continue };
        let name = c.ident.to_string();
        let Some(mib) = name.strip_prefix("MIB_ID_") else {
            continue;
        };
        let id = number(&c.expr).context("expected literal MIB ID")?;
        ensure!(
            rows.insert(id, (mib.to_owned(), doc(&c.attrs))).is_none(),
            "duplicate MIB ID"
        );
    }
    ensure!(!rows.is_empty(), "missing MIB IDs");
    let mut md = String::from("| MIB | Name | Purpose |\n|---:|---|---|\n");
    for (id, (name, purpose)) in rows {
        writeln!(md, "| `0x{id:04x}` | `{name}` | {purpose} |")?;
    }
    md.push_str("{: .messages}\n");
    Ok(md)
}
