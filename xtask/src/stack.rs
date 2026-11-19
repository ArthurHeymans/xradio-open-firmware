//! Bound synchronous call chains separately for the system, IRQ and exception stacks.
//! LLVM supplies frame sizes and disassembly; assembly routines use fixed prologues.
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

#[derive(Default)]
struct Function {
    frame: u32,
    metadata: bool,
    indirect: bool,
    calls: BTreeSet<String>,
}
fn llvm(tool: &str, arg: &str, p: &Path) -> Result<String> {
    let o = Command::new(tool)
        .arg(arg)
        .arg(p)
        .output()
        .with_context(|| format!("running {tool}"))?;
    ensure!(
        o.status.success(),
        "{tool}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    Ok(String::from_utf8(o.stdout)?)
}
fn frames(p: &Path) -> Result<BTreeMap<String, Function>> {
    let mut map = BTreeMap::<String, Function>::new();
    let meta = Regex::new(r"(?s)Entry \{\s*Functions: \[([^\]]+)\]\s*Size: 0x([0-9a-fA-F]+)")?;
    for c in meta.captures_iter(&llvm("llvm-readobj", "--stack-sizes", p)?) {
        for name in c[1].split(", ") {
            let f = map.entry(name.into()).or_default();
            f.frame = u32::from_str_radix(&c[2], 16)?;
            f.metadata = true;
        }
    }
    ensure!(!map.is_empty(), "missing LLVM stack sizes");
    let header = Regex::new(r"^[0-9a-f]+ <([^>]+)>:")?;
    let call = Regex::new(r"\bblx?\s+0x[0-9a-f]+\s+<([^>]+)>")?;
    let indirect = Regex::new(r"\bblx?\s+r\d+\b")?;
    let push = Regex::new(r"\bpush\s+\{([^}]+)\}")?;
    let sub = Regex::new(r"\bsub\s+sp,\s*#0x([0-9a-f]+)")?;
    let dis = llvm("llvm-objdump", "-d", p)?;
    let mut current = None;
    let mut lines = 0;
    for line in dis.lines() {
        if let Some(c) = header.captures(line) {
            let name = c.get(1).unwrap().as_str();
            map.entry(name.into()).or_default();
            current = Some(name);
            lines = 0;
            continue;
        }
        let Some(name) = current else { continue };
        let f = map.get_mut(name).unwrap();
        if let Some(c) = call.captures(line) {
            let target = c[1].split("+0x").next().unwrap();
            if target != name {
                f.calls.insert(target.into());
            }
        } else if indirect.is_match(line) {
            f.indirect = true;
        }
        if !f.metadata && lines < 16 {
            if let Some(c) = push.captures(line) {
                for r in c[1].split(',').map(str::trim) {
                    let n = if let Some((lo, hi)) = r.split_once('-') {
                        let reg = |s: &str| -> Result<u32> {
                            Ok(s.strip_prefix('r').context("register range")?.parse()?)
                        };
                        reg(hi)?
                            .checked_sub(reg(lo)?)
                            .context("bad register range")?
                            + 1
                    } else {
                        1
                    };
                    f.frame += 4 * n;
                }
            }
            if let Some(c) = sub.captures(line) {
                f.frame += u32::from_str_radix(&c[1], 16)?;
            }
        }
        lines += 1;
    }
    Ok(map)
}
fn depth<'a>(
    name: &'a str,
    map: &'a BTreeMap<String, Function>,
    active: &mut BTreeSet<&'a str>,
    memo: &mut BTreeMap<&'a str, u32>,
) -> Result<u32> {
    if let Some(&size) = memo.get(name) {
        return Ok(size);
    }
    ensure!(active.insert(name), "recursive call to {name}");
    let f = map
        .get(name)
        .with_context(|| format!("unknown function {name}"))?;
    if f.indirect {
        bail!("indirect call in {name}")
    };
    let mut child = 0;
    for callee in &f.calls {
        child = child.max(depth(callee, map, active, memo)?);
    }
    active.remove(name);
    let n = f.frame.checked_add(child).context("stack size overflow")?;
    memo.insert(name, n);
    Ok(n)
}
pub fn check(p: &Path) -> Result<()> {
    let map = frames(p)?;
    for (name, veneer, limit) in [
        ("kmain", 0, 0x1b00),
        ("_irq_handler", 24, 256),
        ("xr819_exception_terminal", 56, 256),
    ] {
        let mut reached = BTreeMap::new();
        let bytes = depth(name, &map, &mut BTreeSet::new(), &mut reached)? + veneer;
        ensure!(bytes <= limit, "{name}: {bytes} exceeds {limit}-byte stack");
        if name == "kmain" {
            ensure!(
                reached.keys().any(
                    |n| format!("{:#}", rustc_demangle::demangle(n)).contains("Firmware>::run")
                ),
                "main loop unreachable"
            );
        }
        println!("{name}: {bytes}/{limit} stack bytes");
    }
    Ok(())
}
