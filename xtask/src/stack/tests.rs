use super::*;
#[test]
fn reads_multi_byte_frames_and_rejects_truncation() {
    assert_eq!(stack_sizes(&[5, 0, 0, 0, 0xb0, 1]).unwrap()[&4], 176);
    for bad in [
        &[][..],
        &[0, 0, 0],
        &[0, 0, 0, 0, 0x80],
        &[0, 0, 0, 0, 4, 0, 0, 0, 0, 8],
    ] {
        assert!(stack_sizes(bad).is_err());
    }
}
#[test]
fn excludes_literal_pools_and_follows_direct_calls() {
    // Thumb BL from 0x100 to 0x108, then literal data resembling an indirect call.
    let modes = Map::from([(0x100, ParseMode::Thumb), (0x104, ParseMode::Data)]);
    assert_eq!(
        edges(&[0, 0xf0, 2, 0xf8, 0x80, 0x47], 0x100, &modes).unwrap(),
        BTreeSet::from([0x108])
    );
    let arm = Map::from([(0, ParseMode::Arm)]);
    assert_eq!(
        edges(&[0, 0, 0, 0xeb], 0, &arm).unwrap(),
        BTreeSet::from([8])
    );
}
#[test]
fn unresolved_call_is_not_a_leaf() {
    let modes = Map::from([(0, ParseMode::Thumb)]);
    assert!(edges(&[0x80, 0x47], 0, &modes).is_err());
    assert!(edges(&[0, 0xf0], 0, &modes).is_err());
}
#[test]
fn recursive_bl_is_not_an_interior_branch() {
    let graph = Graph {
        functions: Map::from([(
            0,
            Function {
                name: "f",
                bytes: &[0xff, 0xf7, 0xfe, 0xff],
            },
        )]),
        modes: Map::from([(0, ParseMode::Thumb)]),
        sizes: Map::from([(0, 4)]),
    };
    assert!(
        graph
            .depth(0, &mut BTreeSet::new(), &mut Map::new())
            .unwrap_err()
            .to_string()
            .contains("recursive")
    );
}
#[test]
fn rejects_changed_metadata_free_frame() {
    let b = [
        0, 0xb5, 0x81, 0xb0, 0x6a, 0x46, 0, 0xf0, 0, 0xf8, 0, 0x99, 1, 0xb0, 0, 0xbd,
    ];
    assert_eq!(assembly_frame("__aeabi_uidivmod", &b).unwrap(), 8);
    let mut changed = b;
    changed[2] = 0x82;
    assert!(assembly_frame("__aeabi_uidivmod", &changed).is_err());
    assert!(assembly_frame("unknown", &b).is_err());
}
