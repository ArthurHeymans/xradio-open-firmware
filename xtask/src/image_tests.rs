//! One four-byte executable load and a configurable section. This keeps the
//! image contract testable on the host without compiling an ARM firmware.
use super::image::{bootstrap, runtime};
use object::elf::{SHF_ALLOC, SHT_NOBITS, SHT_PROGBITS};
fn fixture(base: u32, section: (&str, u32, u32, u32)) -> Vec<u8> {
    let (name, kind, flags, addr) = section;
    let names = format!("\0{name}\0.shstrtab\0");
    let mut data = vec![0; 256];
    data[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
    let word = |d: &mut [u8], i: usize, v: u32| d[i..i + 4].copy_from_slice(&v.to_le_bytes());
    for (i, v) in [
        (16, 2u16),
        (18, 40),
        (40, 52),
        (42, 32),
        (44, 1),
        (46, 40),
        (48, 3),
        (50, 2),
    ] {
        data[i..i + 2].copy_from_slice(&v.to_le_bytes());
    }
    for (i, v) in [(20, 1), (24, base), (28, 52), (32, 128)] {
        word(&mut data, i, v);
    }
    for (i, v) in [1, 256, base, base, 4, 4, 5, 4].into_iter().enumerate() {
        word(&mut data, 52 + 4 * i, v);
    }
    for (i, v) in [1, kind, flags, addr, 256, 4, 0, 0, 4, 0]
        .into_iter()
        .enumerate()
    {
        word(&mut data, 168 + 4 * i, v);
    }
    for (i, v) in [
        (name.len() + 2) as u32,
        3,
        0,
        0,
        260,
        names.len() as u32,
        0,
        0,
        1,
        0,
    ]
    .into_iter()
    .enumerate()
    {
        word(&mut data, 208 + 4 * i, v);
    }
    data.extend([12, 34, 56, 78]);
    data.extend(names.bytes());
    data
}
const ALLOC: u32 = SHF_ALLOC;
#[test]
fn packet_sections_are_unloaded() {
    let section = |kind, flags, addr| fixture(0, (".packet_ram.fixture", kind, flags, addr));
    assert!(runtime(&section(SHT_NOBITS, ALLOC, 0x09007000)).is_ok());
    for (kind, flags, addr) in [
        (SHT_PROGBITS, ALLOC, 0x09007000),
        (SHT_NOBITS, 0, 0x09007000),
        (SHT_NOBITS, ALLOC, 0),
    ] {
        assert!(runtime(&section(kind, flags, addr)).is_err());
    }
}
#[test]
fn dtcm_ranges_and_load_coverage() {
    for (base, name, ty, addr, ok) in [
        (0, ".stacks", SHT_NOBITS, 0x0400a000, true),
        (0, ".bss", SHT_NOBITS, 0x04001000, true),
        (0, ".bss", SHT_NOBITS, 0x0400a000, false),
        (0, ".stacks", SHT_NOBITS, 0x0400c000, false),
        (0, ".data", SHT_PROGBITS, 0x04000000, false),
        (0x04000000, ".data", SHT_PROGBITS, 0x04000000, true),
        (0x04000000, ".bss", SHT_NOBITS, 0x04000000, false),
        (0x0400c000, ".text", SHT_PROGBITS, 0x0400c000, false),
        (0x09010000, ".text", SHT_PROGBITS, 0x09010000, false),
    ] {
        assert_eq!(
            runtime(&fixture(base, (name, ty, ALLOC, addr))).is_ok(),
            ok,
            "{name} at {addr:#x}, load {base:#x}"
        );
    }
}
#[test]
fn bootstrap_uses_physical_not_virtual_address() {
    let mut data = fixture(0x08000000, (".entry", SHT_PROGBITS, ALLOC, 0x08000000));
    data[60..64].copy_from_slice(&0x09010000u32.to_le_bytes());
    assert_eq!(bootstrap(&data).unwrap(), [12, 34, 56, 78]);
    data[64..68].copy_from_slice(&0x08000010u32.to_le_bytes());
    assert!(bootstrap(&data).is_err());
}
