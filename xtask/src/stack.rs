//! Static regression check for this linked ARMv5TE image, not a proof for arbitrary ELF files.
use anyhow::{Context, Result, bail, ensure};
use object::{
    Architecture, Object, ObjectSection, ObjectSymbol, SymbolKind,
    read::{Bytes, elf::ElfFile32},
};
use std::collections::{BTreeMap, BTreeSet};
use unarm::{BlxTarget, Ins, Options, ParseEndian, ParseMode, Parser, Version};

#[cfg(test)]
mod tests;

type Map<T> = BTreeMap<u32, T>;

fn stack_sizes(bytes: &[u8]) -> Result<Map<u32>> {
    let mut data = Bytes(bytes);
    let mut frames = Map::new();
    while !data.is_empty() {
        let addr = u32::from_le_bytes(
            *data
                .read::<[u8; 4]>()
                .ok()
                .context("truncated stack address")?,
        ) & !1;
        let frame = u32::try_from(data.read_uleb128().ok().context("invalid stack size")?)?;
        ensure!(
            frames.insert(addr, frame).is_none_or(|old| old == frame),
            "conflicting stack sizes at {addr:#x}"
        );
    }
    ensure!(!frames.is_empty(), "missing stack sizes");
    Ok(frames)
}

/// The only reachable functions in the release ELF lacking compiler frame sizes are two
/// assembly division helpers. Verify their entire Thumb body except the BL displacement
/// (the decoder follows that call below). A changed implementation needs a fresh audit.
fn assembly_frame(name: &str, bytes: &[u8]) -> Result<u32> {
    let (prefix, suffix, size): (&[u8], &[u8], u32) = match name {
        "__aeabi_uidivmod" => (
            &[0x00, 0xb5, 0x81, 0xb0, 0x6a, 0x46],
            &[0, 0x99, 1, 0xb0, 0, 0xbd],
            8,
        ),
        "__aeabi_uldivmod" => (
            &[0x10, 0xb5, 0x84, 0xb0, 2, 0xac, 0, 0x94],
            &[2, 0x9a, 3, 0x9b, 4, 0xb0, 0x10, 0xbd],
            24,
        ),
        _ => bail!("no compiler stack size for {name}"),
    };
    ensure!(
        bytes.len() == prefix.len() + 4 + suffix.len()
            && bytes.starts_with(prefix)
            && bytes.ends_with(suffix),
        "changed assembly frame in {name}"
    );
    let call = &bytes[prefix.len()..prefix.len() + 4];
    ensure!(
        call[1] & 0xf8 == 0xf0 && call[3] & 0xf8 == 0xf8,
        "expected Thumb BL in {name}"
    );
    Ok(size)
}

fn edges(code: &[u8], start: u32, modes: &Map<ParseMode>) -> Result<BTreeSet<u32>> {
    let range = start
        ..start
            .checked_add(u32::try_from(code.len())?)
            .context("function end overflow")?;
    let mut addr = start;
    let mut calls = BTreeSet::new();
    while addr < range.end {
        let end = modes
            .range(addr + 1..range.end)
            .next()
            .map_or(range.end, |(&a, _)| a);
        let mode = *modes
            .range(..=addr)
            .next_back()
            .context("missing ARM ELF mapping symbol")?
            .1;
        if mode != ParseMode::Data {
            let part = &code[(addr - start) as usize..(end - start) as usize];
            let width = if mode == ParseMode::Arm { 4 } else { 2 };
            ensure!(
                addr % width == 0 && end % width == 0,
                "unaligned instruction at {addr:#x}"
            );
            let mut parser = Parser::new(
                part,
                mode,
                ParseEndian::Little,
                Options {
                    version: Version::V5Te,
                    ..Default::default()
                },
            );
            parser.set_pc(addr);
            while let Some(ins) = parser.next() {
                match ins {
                    // Thumb-1 uses BL as a long intra-function branch, too. A BL
                    // to the entry itself, unlike a basic block, is recursion.
                    Ins::Bl { target, .. }
                    | Ins::Blx {
                        target: BlxTarget::Direct(target),
                        ..
                    } if target.addr == start || !range.contains(&target.addr) => {
                        calls.insert(target.addr & !1);
                    }
                    // Conservatively charge the caller's frame on an inter-function tail branch.
                    Ins::B { target, .. } if !range.contains(&target.addr) => {
                        calls.insert(target.addr & !1);
                    }
                    Ins::Blx {
                        target: BlxTarget::Indirect(_),
                        ..
                    } => bail!("indirect call near {:#x}", parser.pc()),
                    Ins::Bx { rm, .. } if rm != unarm::Reg::Lr => {
                        bail!("unresolved register branch near {:#x}", parser.pc())
                    }
                    Ins::Illegal => bail!("undecodable instruction near {:#x}", parser.pc()),
                    _ => {}
                }
            }
            ensure!(parser.pc() == end, "truncated instruction near {end:#x}");
        }
        addr = end;
    }
    Ok(calls)
}

struct Function<'a> {
    name: &'a str,
    bytes: &'a [u8],
}
struct Graph<'a> {
    functions: Map<Function<'a>>,
    sizes: Map<u32>,
    modes: Map<ParseMode>,
}
impl Graph<'_> {
    fn depth(&self, addr: u32, active: &mut BTreeSet<u32>, seen: &mut Map<u32>) -> Result<u32> {
        if let Some(&size) = seen.get(&addr) {
            return Ok(size);
        }
        let f = self
            .functions
            .get(&addr)
            .with_context(|| format!("unresolved call target {addr:#x}"))?;
        ensure!(active.insert(addr), "recursive call to {}", f.name);
        let size = match self.sizes.get(&addr) {
            Some(&n) => n,
            None => assembly_frame(f.name, f.bytes)?,
        };
        let mut child = 0;
        for dst in edges(f.bytes, addr, &self.modes).with_context(|| f.name.to_owned())? {
            child = child.max(self.depth(dst, active, seen)?);
        }
        let total = size
            .checked_add(child)
            .context("stack overflow in analysis")?;
        active.remove(&addr);
        seen.insert(addr, total);
        Ok(total)
    }
}

pub fn check(data: &[u8]) -> Result<()> {
    let elf = ElfFile32::<object::LittleEndian>::parse(data)?;
    ensure!(
        elf.architecture() == Architecture::Arm && elf.is_little_endian(),
        "expected ARM little-endian ELF32"
    );
    let text = elf.section_by_name(".text").context("missing .text")?;
    let sizes = stack_sizes(
        elf.section_by_name(".stack_sizes")
            .context("missing .stack_sizes")?
            .data()?,
    )?;
    let mut modes = Map::new();
    let mut functions = Map::new();
    let mut symbols = BTreeMap::new();
    for s in elf.symbols() {
        let name = s.name()?;
        let addr = s.address() as u32;
        symbols.insert(name, addr);
        if s.section_index() != Some(text.index()) {
            continue;
        }
        if let Some(mode) = ParseMode::from_mapping_symbol(name.split('.').next().unwrap()) {
            modes.insert(addr, mode);
            continue;
        }
        if s.kind() != SymbolKind::Text || s.size() == 0 {
            continue;
        }
        let addr = addr & !1;
        let bytes = text
            .data_range(addr.into(), s.size())?
            .context("function outside .text")?;
        functions.insert(addr, Function { name, bytes });
    }
    let symbol = |name: &str| {
        symbols
            .get(name)
            .copied()
            .with_context(|| format!("missing symbol {name}"))
    };
    let graph = Graph {
        functions,
        sizes,
        modes,
    };
    for (root, save, lo, hi) in [
        ("kmain", 0, "_sys_stack_low_end", "_sys_stack_high_end"),
        (
            "_irq_handler",
            24,
            "_irq_stack_low_end",
            "_irq_stack_high_end",
        ),
        (
            "xr819_exception_terminal",
            56,
            "_abt_stack_low_end",
            "_abt_stack_high_end",
        ),
    ] {
        let limit = symbol(hi)?
            .checked_sub(symbol(lo)?)
            .context("invalid stack range")?;
        let mut seen = Map::new();
        let n = graph
            .depth(symbol(root)? & !1, &mut BTreeSet::new(), &mut seen)?
            .checked_add(save)
            .context("entry frame overflow")?;
        ensure!(n <= limit, "{root}: {n} exceeds {limit}-byte stack");
        if root == "kmain" {
            ensure!(
                seen.keys().any(|a| format!(
                    "{:#}",
                    rustc_demangle::demangle(graph.functions[a].name)
                )
                .contains("Firmware>::run")),
                "main loop unreachable"
            );
        }
        println!("{root}: {n}/{limit} stack bytes");
    }
    Ok(())
}
