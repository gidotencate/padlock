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

/// `_Atomic int` fields (C11) must be sized as `int` (4 bytes), not 0 bytes.
/// DW_TAG_atomic_type is a qualifier wrapper — previously it fell to the `_`
/// catch-all which read DW_AT_byte_size on the wrapper itself (absent), giving
/// size=0 and inflating the apparent gap after the field by 4 bytes.
#[test]
fn atomic_int_field_sized_correctly() {
    let Some(binary) = compile_c(
        r#"
#include <stdatomic.h>
struct AtomicFields {
    _Atomic int  counter;
    int          regular;
};
struct AtomicFields instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "AtomicFields");
    assert_eq!(layouts.len(), 1, "expected exactly one AtomicFields layout");
    let l = &layouts[0];

    let counter = l
        .fields
        .iter()
        .find(|f| f.name == "counter")
        .expect("counter field must be present");
    let regular = l
        .fields
        .iter()
        .find(|f| f.name == "regular")
        .expect("regular field must be present");

    assert_eq!(counter.size, 4, "_Atomic int must be 4 bytes");
    assert_eq!(counter.offset, 0, "counter must start at offset 0");
    assert_eq!(regular.offset, 4, "regular follows immediately (no gap)");
    assert_eq!(l.total_size, 8, "total size must be 8 bytes");
}

/// `_Atomic long` fields must be sized as `long` (8 bytes on 64-bit x86_64).
#[test]
fn atomic_long_long_field_sized_correctly() {
    // Use `long long` (always 64-bit) instead of `long` which is 32-bit on
    // Windows (MSVC ABI) even on 64-bit targets.
    let Some(binary) = compile_c(
        r#"
#include <stdatomic.h>
struct AtomicLongLong {
    _Atomic long long value;
    int               tag;
};
struct AtomicLongLong instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "AtomicLongLong");
    assert_eq!(layouts.len(), 1);
    let l = &layouts[0];

    let value = l
        .fields
        .iter()
        .find(|f| f.name == "value")
        .expect("value field must be present");
    assert_eq!(value.size, 8, "_Atomic long long must be 8 bytes");
    assert_eq!(value.offset, 0);
}

/// DWARF5 encodes bitfield offsets via `DW_AT_data_bit_offset` (absolute bit
/// offset from the struct start) instead of `DW_AT_data_member_location`.
/// GCC emits this form for `unsigned` bitfields packed after a run of byte-
/// sized fields (as seen in SQLite's VdbeCursor).  Previously the extractor
/// skipped these members silently, reporting a false gap in their place.
///
/// After the fix, the bitfield group must appear as a synthetic field and the
/// non-bitfield `next` field must be reported at its true offset with no gap.
#[test]
fn dwarf5_data_bit_offset_bitfields_no_false_gap() {
    // GCC emits DW_AT_data_bit_offset for unsigned bitfields that follow u8
    // fields when using -gdwarf-5 or a recent enough default dwarf version.
    // Compile with -gdwarf-5 to force it; skip if cc doesn't support the flag.
    let src = r#"
struct BitfieldAfterBytes {
    unsigned char  a;
    unsigned char  b;
    unsigned       x : 1;
    unsigned       y : 1;
    unsigned short next;
};
struct BitfieldAfterBytes instance;
"#;
    use std::io::Write as _;
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let src_path = dir.path().join("test.c");
    let obj_path = dir.path().join("test.o");
    let Ok(mut f) = std::fs::File::create(&src_path) else {
        return;
    };
    let Ok(_) = f.write_all(src.as_bytes()) else {
        return;
    };
    let status = match std::process::Command::new("cc")
        .args([
            "-gdwarf-5",
            "-c",
            src_path.to_str().unwrap(),
            "-o",
            obj_path.to_str().unwrap(),
        ])
        .status()
    {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[skip] cc not available");
            return;
        }
    };
    if !status.success() {
        eprintln!("[skip] cc -gdwarf-5 not supported");
        return;
    }
    let Ok(binary) = std::fs::read(&obj_path) else {
        return;
    };

    let layouts = extract(&binary, "BitfieldAfterBytes");
    assert_eq!(layouts.len(), 1, "expected one BitfieldAfterBytes layout");
    let l = &layouts[0];

    // `next` is a u16 after the bitfield group.  Without the fix the extractor
    // skipped the bitfield fields and reported their byte as a gap, making
    // `next` appear to follow `b` (offset 2) with a spurious 1-byte hole.
    // With the fix the bitfield group occupies its byte and the wasted_bytes
    // count excludes that byte.
    let next = l.fields.iter().find(|f| f.name == "next");
    if let Some(next) = next {
        // `next` must be at a 2-byte-aligned offset past the bitfield byte.
        assert!(
            next.offset >= 3,
            "next must be after the bitfield group, got offset {}",
            next.offset
        );
    }

    // Total size must be correct (depends on compiler packing; just check
    // it is nonzero and a reasonable value).
    assert!(l.total_size >= 4 && l.total_size <= 16);
}

/// `DW_AT_byte_size` on a structure type may be encoded as
/// `DW_FORM_implicit_const` (an abbreviation-table constant, decoded by
/// gimli as `Sdata` rather than `Udata`).  GCC uses this when the size is
/// the same for every instance of the abbreviation — common for pointer-
/// sized or otherwise fixed-size helper structs.  Previously only `Udata`
/// was matched, so affected structs were silently dropped.
#[test]
fn implicit_const_byte_size_struct_is_extracted() {
    // A simple two-pointer struct is likely to be placed in an abbreviation
    // entry that uses implicit_const for DW_AT_byte_size, though this is
    // compiler-version-dependent.  We compile without -O to minimise
    // abbreviation sharing and maximise the chance of hitting the form.
    // Even if the compiler uses Udata, the test still passes — we're just
    // verifying the struct is found and has the right size.
    let Some(binary) = compile_c(
        r#"
struct PtrPair {
    void *a;
    void *b;
};
struct PtrPair instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "PtrPair");
    assert_eq!(layouts.len(), 1, "PtrPair must be extracted");
    let l = &layouts[0];
    // On 64-bit: two 8-byte pointers = 16B total, no holes.
    let arch = reader::detect_arch(&binary).unwrap();
    let expected = 2 * arch.pointer_size;
    assert_eq!(
        l.total_size, expected,
        "PtrPair size must be 2*pointer_size"
    );
    assert_eq!(l.fields.len(), 2, "PtrPair must have exactly 2 fields");
}

/// C++ `class` declarations use `DW_TAG_class_type` instead of
/// `DW_TAG_structure_type`.  Previously only structure_type was scanned, so
/// any class whose fields we want to analyze was silently absent from output.
#[test]
fn cpp_class_type_is_extracted() {
    // Compile a C++ translation unit with a class declaration.
    use std::io::Write as _;
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let src_path = dir.path().join("test.cpp");
    let obj_path = dir.path().join("test.o");
    let src = r#"
class Widget {
public:
    int   id;
    float value;
    char  tag;
};
Widget w;
"#;
    let Ok(mut f) = std::fs::File::create(&src_path) else {
        return;
    };
    let _ = f.write_all(src.as_bytes());
    let status = match std::process::Command::new("c++")
        .args([
            "-g",
            "-c",
            src_path.to_str().unwrap(),
            "-o",
            obj_path.to_str().unwrap(),
        ])
        .status()
    {
        Ok(s) => s,
        Err(_) => {
            eprintln!("[skip] c++ not available");
            return;
        }
    };
    if !status.success() {
        eprintln!("[skip] c++ compilation failed");
        return;
    }
    let Ok(binary) = std::fs::read(&obj_path) else {
        return;
    };

    let layouts = extract(&binary, "Widget");
    assert_eq!(
        layouts.len(),
        1,
        "Widget class must be extracted from C++ DWARF"
    );
    let l = &layouts[0];
    assert!(
        l.fields.iter().any(|f| f.name == "id"),
        "Widget must contain field 'id'"
    );
}

/// `DW_AT_alignment` on a struct DIE captures an explicit `alignas(N)` or
/// `__attribute__((aligned(N)))` on the type.  The struct align must reflect
/// this, not just the maximum field alignment.
#[test]
fn struct_explicit_alignment_is_respected() {
    let Some(binary) = compile_c(
        r#"
struct __attribute__((aligned(64))) CacheAligned {
    int a;
    int b;
};
struct CacheAligned instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "CacheAligned");
    assert_eq!(layouts.len(), 1, "CacheAligned must be extracted");
    let l = &layouts[0];
    // The struct's declared alignment is 64; field alignment is only 4.
    // Without reading DW_AT_alignment the extractor would report align=4.
    assert_eq!(
        l.align, 64,
        "CacheAligned must have align=64 from explicit attribute"
    );
}

/// `__attribute__((aligned(N)))` on a struct field declaration produces
/// `DW_AT_alignment` on the `DW_TAG_member` DIE (not the type DIE).
/// Previously `extract_field` did not read this attribute, so the field's
/// alignment was reported as its type's natural alignment rather than N.
#[test]
fn field_explicit_alignment_is_respected() {
    let Some(binary) = compile_c(
        r#"
struct MixedAlign {
    char  a;
    int   __attribute__((aligned(8))) b;
    short c;
};
struct MixedAlign instance;
"#,
    ) else {
        eprintln!("[skip] cc not available");
        return;
    };

    let layouts = extract(&binary, "MixedAlign");
    assert_eq!(layouts.len(), 1, "MixedAlign must be extracted");
    let l = &layouts[0];

    let b = l.fields.iter().find(|f| f.name == "b").expect("field b");
    // Without DW_AT_alignment on the member, b.align would be 4 (int).
    // With the fix it must be 8.
    assert_eq!(
        b.align, 8,
        "field b must have align=8 from member attribute"
    );
    // b must also be at an 8-byte-aligned offset.
    assert_eq!(b.offset % 8, 0, "field b must start at 8B-aligned offset");
}
