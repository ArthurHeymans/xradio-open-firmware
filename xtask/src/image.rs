use anyhow::{Context, Result, ensure};
use object::{
    Architecture, Object, ObjectSection, SectionFlags, elf,
    read::elf::{ElfFile32, ProgramHeader},
};
use std::ops::Range;

struct Load<'a> {
    base: u32,
    end: u32,
    bytes: &'a [u8],
    executable: bool,
}
fn intersects(a: &Range<u32>, b: &Range<u32>) -> bool {
    a.start < b.end && b.start < a.end
}
fn contains(a: &Range<u32>, b: &Range<u32>) -> bool {
    a.start <= b.start && b.end <= a.end
}

fn loads(data: &[u8]) -> Result<(ElfFile32<'_>, Vec<Load<'_>>)> {
    let file = ElfFile32::parse(data)?;
    ensure!(
        file.architecture() == Architecture::Arm
            && file.is_little_endian()
            && file.kind() == object::ObjectKind::Executable,
        "expected a little-endian ARM ELF32 executable"
    );
    let endian = file.endian();
    let mut loads = Vec::new();
    for ph in file.elf_program_headers() {
        if ph.p_type(endian) != elf::PT_LOAD {
            continue;
        }
        let mem = ph.p_memsz(endian);
        let bytes = ph
            .data(endian, data)
            .map_err(|_| anyhow::anyhow!("invalid segment payload"))?;
        ensure!(
            bytes.len() <= mem as usize,
            "segment payload exceeds memory size"
        );
        if mem == 0 {
            continue;
        }
        let base = match ph.p_paddr(endian) {
            0 => ph.p_vaddr(endian),
            p => p,
        };
        ensure!(base % 4 == 0 && bytes.len() % 4 == 0, "unaligned load");
        let end = base
            .checked_add(mem.checked_next_multiple_of(4).context("range overflow")?)
            .context("range overflow")?;
        ensure!(
            !loads
                .iter()
                .any(|s: &Load| intersects(&(base..end), &(s.base..s.end))),
            "overlapping loads"
        );
        loads.push(Load {
            base,
            end,
            bytes,
            executable: ph.p_flags(endian) & elf::PF_X != 0,
        });
    }
    ensure!(!loads.is_empty(), "no loadable segments");
    Ok((file, loads))
}
fn words(out: &mut Vec<u8>, values: &[u32]) {
    for v in values {
        out.extend(v.to_le_bytes());
    }
}
fn copy(out: &mut Vec<u8>, addr: u32, data: &[u8]) {
    if !data.is_empty() {
        words(out, &[0, addr, data.len() as u32]);
        out.extend(data);
    }
}
fn fill(out: &mut Vec<u8>, addr: u32, len: u32) {
    if len != 0 {
        words(out, &[1, addr, 0, len]);
    }
}
fn encode(out: &mut Vec<u8>, s: &Load) {
    let mut start = 0;
    let mut i = 0;
    while i < s.bytes.len() {
        let zero = i;
        while s.bytes.get(i..i + 4) == Some(&[0; 4]) {
            i += 4;
        }
        if i - zero >= 64 {
            copy(out, s.base + start as u32, &s.bytes[start..zero]);
            fill(out, s.base + zero as u32, (i - zero) as u32);
            start = i;
        }
        if zero == i {
            i += 4;
        }
    }
    copy(out, s.base + start as u32, &s.bytes[start..]);
    let tail = s.base + s.bytes.len() as u32;
    fill(out, tail, s.end - tail);
}
pub fn runtime(data: &[u8]) -> Result<Vec<u8>> {
    let (file, loads) = loads(data)?;
    let alias = 0x04000000..0x04010000;
    let state = alias.start..0x0400a000;
    let stacks = state.end..0x0400c000;
    let entry = file.entry() as u32;
    ensure!(
        loads.iter().any(
            |s| s.executable && (s.base..s.base + s.bytes.len() as u32).contains(&(entry & !1))
        ),
        "entry outside executable payload"
    );
    for s in &loads {
        let r = s.base..s.end;
        ensure!(
            !intersects(&r, &(0x09000000..0x0a000000))
                && (!intersects(&r, &alias) || contains(&state, &r)),
            "load in reserved memory"
        );
    }
    ensure!(file.sections().next().is_some(), "missing section table");
    for sec in file.sections() {
        let name = sec.name()?;
        let a = sec.address() as u32;
        let r = a..a
            .checked_add(sec.size() as u32)
            .context("section overflow")?;
        let allocated = matches!(sec.flags(), SectionFlags::Elf{sh_flags} if sh_flags & u64::from(elf::SHF_ALLOC)!=0);
        let nobits = sec.file_range().is_none();
        let dtcm = allocated && !r.is_empty() && intersects(&r, &alias);
        if dtcm {
            ensure!(
                contains(if name == ".stacks" { &stacks } else { &state }, &r),
                "{name} outside allowed DTCM"
            );
        }
        if name.starts_with(".packet_ram.") || (dtcm && nobits) {
            ensure!(
                allocated && nobits && !loads.iter().any(|s| intersects(&r, &(s.base..s.end))),
                "invalid NOLOAD section {name}"
            );
        } else if dtcm {
            ensure!(
                loads
                    .iter()
                    .any(|s| contains(&(s.base..s.base + s.bytes.len() as u32), &r)),
                "unbacked DTCM section {name}"
            );
        }
    }
    let mut out = b"XR01".to_vec();
    for s in &loads {
        encode(&mut out, s);
    }
    words(&mut out, &[4, entry, 0]);
    Ok(out)
}
pub fn bootstrap(data: &[u8]) -> Result<Vec<u8>> {
    let (file, mut loads) = loads(data)?;
    let base = 0x08000000;
    ensure!(file.entry() == base as u64, "unexpected bootstrap entry");
    loads.sort_by_key(|s| s.base);
    let mut out = Vec::new();
    for s in loads {
        ensure!(
            s.base == base + out.len() as u32,
            "non-contiguous bootstrap load addresses"
        );
        out.extend(s.bytes);
    }
    ensure!(!out.is_empty(), "empty bootstrap");
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splits_long_zero_runs_and_fills_bss() {
        let mut bytes = vec![7; 4];
        bytes.extend([0; 64]);
        bytes.extend([9; 4]);
        let mut out = Vec::new();
        encode(
            &mut out,
            &Load {
                base: 16,
                end: 92,
                bytes: &bytes,
                executable: false,
            },
        );
        let mut expect = Vec::new();
        words(
            &mut expect,
            &[
                0, 16, 4, 0x07070707, 1, 20, 0, 64, 0, 84, 4, 0x09090909, 1, 88, 0, 4,
            ],
        );
        assert_eq!(out, expect);
    }
    #[test]
    fn keeps_short_zero_run() {
        let mut out = Vec::new();
        encode(
            &mut out,
            &Load {
                base: 0,
                end: 60,
                bytes: &[0; 60],
                executable: false,
            },
        );
        assert_eq!(out.len(), 12 + 60);
    }
}
