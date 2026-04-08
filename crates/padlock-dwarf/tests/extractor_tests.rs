// padlock-dwarf/tests/extractor_tests.rs
//
// Integration tests for the DWARF extractor. These tests compile small C
// snippets on the fly (using `cc -g -c`) and then verify that padlock-dwarf
// extracts the expected struct layouts from the resulting object file.
//
// Tests that need a C compiler are guarded by `compile_c`: if `cc` is not on
// PATH the helper returns `None` and the test exits early with a printed notice
// rather than failing. On Linux (including CI runners), `cc` is always present.

use padlock_dwarf::{extractor::Extractor, reader};

// ── compiler helper ────────────────────────────────────────────────────────────

/// Write `src` to a temp file, compile it with `cc -g -c`, and return the
/// resulting object-file bytes.  Returns `None` if compilation fails or the
/// `cc` binary is not available.
fn compile_c(src: &str) -> Option<Vec<u8>> {
    use std::io::Write as _;
    let dir = tempfile::tempdir().ok()?;
    let src_path = dir.path().join("test.c");
    let obj_path = dir.path().join("test.o");
    std::fs::File::create(&src_path)
        .ok()?
        .write_all(src.as_bytes())
        .ok()?;
    let status = std::process::Command::new("cc")
        .args(["-g", "-c", src_path.to_str()?, "-o", obj_path.to_str()?])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&obj_path).ok()
}

/// Load DWARF from `binary`, extract layouts, and return all layouts whose
/// name equals `struct_name`.
fn extract(binary: &[u8], struct_name: &str) -> Vec<padlock_core::ir::StructLayout> {
    let dwarf = reader::load(binary).expect("load DWARF");
    let arch = reader::detect_arch(binary).expect("detect arch");
    let extractor = Extractor::new(&dwarf, arch);
    extractor
        .extract_all()
        .expect("extract_all")
        .into_iter()
        .filter(|l| l.name == struct_name)
        .collect()
}

// ── tests ──────────────────────────────────────────────────────────────────────

/// The simplest case: field names, sizes, and offsets must match what the
/// C compiler actually produced (which we read back from DWARF).
#[test]
fn extract_simple_struct_field_names_and_offsets() {
    let Some(binary) = compile_c(
        r#"
struct Simple {
    int   a;
    char  b;
    double c;
};
struct Simple instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "Simple");
    assert_eq!(layouts.len(), 1, "expected exactly one Simple struct");
    let l = &layouts[0];

    // Field count
    assert_eq!(l.fields.len(), 3);

    // Field names (sorted by offset, as extractor does)
    assert_eq!(l.fields[0].name, "a");
    assert_eq!(l.fields[1].name, "b");
    assert_eq!(l.fields[2].name, "c");

    // Offsets: int a at 0, char b at 4, double c at 8 (4 bytes padding between b and c)
    assert_eq!(l.fields[0].offset, 0);
    assert_eq!(l.fields[0].size, 4); // int
    assert_eq!(l.fields[1].offset, 4);
    assert_eq!(l.fields[1].size, 1); // char
    assert_eq!(l.fields[2].offset, 8); // aligned to 8
    assert_eq!(l.fields[2].size, 8); // double

    // Total size: double ends at 16, struct align = 8 → 16 bytes
    assert_eq!(l.total_size, 16);
}

/// A struct with no padding: all fields already in natural order.
#[test]
fn extract_packed_natural_struct() {
    let Some(binary) = compile_c(
        r#"
struct Packed {
    int   a;
    int   b;
    short c;
    short d;
};
struct Packed instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "Packed");
    assert_eq!(layouts.len(), 1);
    let l = &layouts[0];
    assert_eq!(l.fields.len(), 4);
    // No holes: a@0, b@4, c@8, d@10 → 12 bytes → aligned to 4 → 12
    assert_eq!(l.fields[0].offset, 0);
    assert_eq!(l.fields[1].offset, 4);
    assert_eq!(l.fields[2].offset, 8);
    assert_eq!(l.fields[3].offset, 10);
    assert_eq!(l.total_size, 12);
}

/// A `typedef struct { ... } Name` must be extracted with the typedef name.
#[test]
fn extract_typedef_struct_name() {
    let Some(binary) = compile_c(
        r#"
typedef struct {
    int x;
    int y;
} Point;
Point origin;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "Point");
    assert_eq!(layouts.len(), 1, "should find typedef name 'Point'");
    let l = &layouts[0];
    assert_eq!(l.name, "Point");
    assert_eq!(l.fields.len(), 2);
}

/// Struct with pointer fields: pointer size must match the target architecture.
#[test]
fn extract_pointer_field_size() {
    let Some(binary) = compile_c(
        r#"
struct Node {
    int         value;
    struct Node *next;
};
struct Node instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "Node");
    assert_eq!(layouts.len(), 1);
    let l = &layouts[0];

    // On x86-64: value@0 (4B), padding 4B, next@8 (8B) → total 16
    let arch = reader::detect_arch(&binary).unwrap();
    let next = l.fields.iter().find(|f| f.name == "next").unwrap();
    assert_eq!(next.size, arch.pointer_size);
}

/// `detect_arch` on a real compiled object file must return a known arch.
#[test]
fn detect_arch_on_real_object() {
    let Some(binary) = compile_c("int x = 0;") else {
        eprintln!("[skip] cc not available");
        return;
    };
    let arch = reader::detect_arch(&binary).unwrap();
    assert!(
        matches!(
            arch.name,
            "x86_64" | "aarch64" | "aarch64-apple" | "riscv64"
        ),
        "unexpected arch: {}",
        arch.name
    );
}

/// Incomplete / forward-declared structs must not appear in the output.
/// (A `DW_AT_declaration` struct has no byte size and must be skipped.)
#[test]
fn forward_declared_struct_not_extracted() {
    let Some(binary) = compile_c(
        r#"
struct Opaque;
struct Container {
    int          id;
    struct Opaque *ptr;
};
struct Container instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    // "Opaque" is forward-declared only; the extractor must skip it.
    let layouts = extract(&binary, "Opaque");
    assert!(
        layouts.is_empty(),
        "forward-declared struct should not be extracted"
    );
    // "Container" must still be extracted correctly.
    let containers = extract(&binary, "Container");
    assert_eq!(containers.len(), 1);
}

/// Bit-field members must be silently dropped from the extracted layout.
/// They share byte offsets with adjacent fields and cannot be represented in
/// the byte-level IR. The remaining non-bit-field members must still appear.
#[test]
fn bitfield_members_are_skipped() {
    let Some(binary) = compile_c(
        r#"
struct Flags {
    unsigned int width  : 10;
    unsigned int height : 10;
    unsigned int flags  : 12;
    int          value;
};
struct Flags instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "Flags");
    assert_eq!(layouts.len(), 1);
    let l = &layouts[0];

    // Bit-field members (width, height, flags) must not appear.
    let bitfield_names = ["width", "height", "flags"];
    for bf in &bitfield_names {
        assert!(
            !l.fields.iter().any(|f| f.name == *bf),
            "bit-field '{bf}' must be absent from extracted layout"
        );
    }

    // The non-bit-field member 'value' must still be present.
    assert!(
        l.fields.iter().any(|f| f.name == "value"),
        "non-bit-field 'value' must be present"
    );
}

/// The padlock analysis passes must produce sensible findings on a real
/// extracted layout (smoke test for the end-to-end pipeline).
#[test]
fn analysis_on_extracted_layout_produces_findings() {
    let Some(binary) = compile_c(
        r#"
struct Wasteful {
    char  flag;
    double value;
    int   count;
};
struct Wasteful instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "Wasteful");
    assert_eq!(layouts.len(), 1);

    let report = padlock_core::findings::Report::from_layouts(&layouts);
    let sr = &report.structs[0];

    // flag@0(1B) + 7B padding + value@8(8B) + count@16(4B) + 4B trailing = 24B
    assert_eq!(sr.total_size, 24);
    assert!(
        sr.wasted_bytes > 0,
        "Wasteful struct must have padding waste"
    );
    assert!(
        sr.findings
            .iter()
            .any(|f| matches!(f, padlock_core::findings::Finding::ReorderSuggestion { .. })),
        "should suggest reordering"
    );
}
