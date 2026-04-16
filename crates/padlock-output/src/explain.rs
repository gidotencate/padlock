// padlock-output/src/explain.rs
//
// Renders a visual field-by-field memory layout table for a single struct.
// Shows each field's offset, size, alignment, and padding gaps inline.

use padlock_core::analysis::impact::estimate_impact;
use padlock_core::ir::{StructLayout, TypeInfo, find_padding};

/// Render a visual layout table for one struct.
///
/// Example output:
///
/// ```text
/// ReadyEvent  24 bytes  align=4
/// ┌──────────────────────────────────────────────────────────────────┐
/// │ offset │ size │ align │ CL │ field                              │
/// ├──────────────────────────────────────────────────────────────────┤
/// │      0 │    1 │     1 │  0 │ tick: u8                          │
/// │      1 │    3 │     — │  0 │ <padding>                         │
/// │      4 │    4 │     4 │  0 │ ready: Ready                      │
/// │      8 │    1 │     1 │  0 │ is_shutdown: bool                 │
/// │      9 │   15 │     — │  0 │ <padding> (trailing)              │
/// └──────────────────────────────────────────────────────────────────┘
/// 14 bytes wasted (58%) — reorder: ready, tick, is_shutdown → 8 bytes
/// ```
///
/// The `CL` column shows the zero-indexed cache-line number for each field.
/// A cache-line separator row (`╞══ cache line N ══╡`) is also emitted
/// whenever the layout crosses a cache-line boundary.
pub fn render_explain(layout: &StructLayout) -> String {
    use padlock_core::analysis::reorder;

    let mut out = String::new();

    // Header
    let loc = match (&layout.source_file, layout.source_line) {
        (Some(f), Some(l)) => format!("  ({}:{})", f, l),
        (Some(f), None) => format!("  ({})", f),
        _ => String::new(),
    };
    out.push_str(&format!("{}{}\n", layout.name, loc));
    out.push_str(&format!(
        "{} bytes  align={}  fields={}{}{}\n",
        layout.total_size,
        layout.align,
        layout.fields.len(),
        if layout.is_packed { "  [packed]" } else { "" },
        if layout.is_repr_rust {
            "  [repr(Rust) — compiler may reorder]"
        } else {
            ""
        },
    ));

    // Table — columns: offset(6) | size(4) | align(5) | CL(2) | field(36)
    let col_field = 36usize;
    let divider = format!(
        "├{:─<8}┼{:─<6}┼{:─<7}┼{:─<4}┼{:─<col_field$}┤",
        "", "", "", "", ""
    );
    let top = format!(
        "┌{:─<8}┬{:─<6}┬{:─<7}┬{:─<4}┬{:─<col_field$}┐",
        "", "", "", "", ""
    );
    let bot = format!(
        "└{:─<8}┴{:─<6}┴{:─<7}┴{:─<4}┴{:─<col_field$}┘",
        "", "", "", "", ""
    );
    let header = format!(
        "│ {:>6} │ {:>4} │ {:>5} │ {:>2} │ {:<col_field$}│",
        "offset", "size", "align", "CL", "field"
    );

    out.push_str(&top);
    out.push('\n');
    out.push_str(&header);
    out.push('\n');
    out.push_str(&divider);
    out.push('\n');

    // Build rows: interleave fields with padding gaps and cache-line markers
    #[derive(Debug)]
    enum Row {
        Field {
            offset: usize,
            size: usize,
            align: usize,
            name: String,
            ty: String,
        },
        Pad {
            offset: usize,
            size: usize,
            trailing: bool,
        },
        CacheLine {
            line_number: usize,
            offset: usize,
        },
    }

    let cache_line = layout.arch.cache_line_size;
    let mut rows: Vec<Row> = Vec::new();
    let gaps = find_padding(layout);

    let last_field_name = layout.fields.last().map(|f| f.name.as_str()).unwrap_or("");

    // Track which cache lines have been crossed so we can insert markers.
    let mut last_cache_line: Option<usize> = None;

    for field in &layout.fields {
        let field_cache_line = field.offset / cache_line;

        // Insert a cache-line boundary marker when entering a new cache line.
        if last_cache_line.is_none_or(|prev| field_cache_line > prev) {
            if last_cache_line.is_some() {
                // Not the first cache line: insert a separator row.
                rows.push(Row::CacheLine {
                    line_number: field_cache_line,
                    offset: field_cache_line * cache_line,
                });
            }
            last_cache_line = Some(field_cache_line);
        }

        let ty_name = type_name(&field.ty);
        rows.push(Row::Field {
            offset: field.offset,
            size: field.size,
            align: field.align,
            name: field.name.clone(),
            ty: ty_name,
        });
        if let Some(gap) = gaps.iter().find(|g| g.after_field == field.name) {
            let pad_offset = field.offset + field.size;
            let is_trailing = field.name == last_field_name;
            rows.push(Row::Pad {
                offset: pad_offset,
                size: gap.bytes,
                trailing: is_trailing,
            });
        }
    }

    // Cache-line separator row width matches the table inner width.
    // Inner width = 8(offset) + 1(┼) + 6(size) + 1(┼) + 7(align) + 1(┼) + 4(CL) + 1(┼) + col_field + 3
    let cache_sep_inner = 8 + 1 + 6 + 1 + 7 + 1 + 4 + 1 + col_field + 3; // ─ count between outer │
    for row in &rows {
        match row {
            Row::Field {
                offset,
                size,
                align,
                name,
                ty,
            } => {
                let cl = offset / cache_line;
                let label = format!("{}: {}", name, ty);
                let label = if label.len() > col_field {
                    format!("{}…", &label[..col_field - 1])
                } else {
                    label
                };
                out.push_str(&format!(
                    "│ {:>6} │ {:>4} │ {:>5} │ {:>2} │ {:<col_field$}│\n",
                    offset, size, align, cl, label
                ));
            }
            Row::Pad {
                offset,
                size,
                trailing,
            } => {
                let cl = offset / cache_line;
                let label = if *trailing {
                    "<padding> (trailing)".to_string()
                } else {
                    "<padding>".to_string()
                };
                out.push_str(&format!(
                    "│ {:>6} │ {:>4} │ {:>5} │ {:>2} │ {:<col_field$}│\n",
                    offset, size, "—", cl, label
                ));
            }
            Row::CacheLine {
                line_number,
                offset,
            } => {
                let label = format!("── cache line {line_number} (offset {offset}) ");
                // Pad to fill the inner width with '═' characters.
                let used = label.len();
                let pad = if cache_sep_inner > used + 4 {
                    "═".repeat(cache_sep_inner - used - 4)
                } else {
                    String::new()
                };
                out.push_str(&format!("╞{label}{pad}╡\n"));
            }
        }
    }

    out.push_str(&bot);
    out.push('\n');

    // Summary line — gaps already includes trailing padding from find_padding.
    let wasted: usize = gaps.iter().map(|g| g.bytes).sum();

    if wasted > 0 && !layout.is_packed && !layout.is_union {
        let pct = wasted as f64 / layout.total_size as f64 * 100.0;
        let (opt_size, savings) = reorder::reorder_savings(layout);
        if savings > 0 {
            let opt_order: Vec<String> = reorder::optimal_order(layout)
                .iter()
                .map(|f| f.name.clone())
                .collect();
            out.push_str(&format!(
                "{} bytes wasted ({:.0}%) — reorder: {} → {} bytes\n",
                wasted,
                pct,
                opt_order.join(", "),
                opt_size
            ));

            // Impact block: show concrete memory/cache effects at scale.
            // Cache line size: default 64 bytes (x86-64 / aarch64).
            const CACHE_LINE: usize = 64;
            let impact = estimate_impact(savings, layout.total_size, opt_size, CACHE_LINE);
            out.push_str(&format!(
                "  ~{savings} KB extra per 1K instances · ~{savings} MB per 1M \
                 instances · ~{cl_1m} extra cache lines/1M (seq. scan)\n",
                cl_1m = fmt_count(impact.extra_cache_lines_1m),
            ));
            if impact.reduces_cache_line_crossings() {
                out.push_str(&format!(
                    "  Spans {} cache line(s); optimal spans {}\n",
                    impact.current_cache_lines, impact.optimal_cache_lines,
                ));
            }
        } else {
            out.push_str(&format!(
                "{} bytes wasted ({:.0}%) — already in optimal order\n",
                wasted, pct
            ));
        }
    } else if layout.is_packed {
        out.push_str("packed — no padding\n");
    } else {
        out.push_str("no padding waste\n");
    }

    out
}

/// Format a large count with K/M suffix for readability.
fn fmt_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

fn type_name(ty: &TypeInfo) -> String {
    match ty {
        TypeInfo::Primitive { name, .. } => name.clone(),
        TypeInfo::Pointer { .. } => "*ptr".to_string(),
        TypeInfo::Array { element, count, .. } => format!("[{}; {}]", type_name(element), count),
        TypeInfo::Struct(inner) => inner.name.clone(),
        TypeInfo::Opaque { name, .. } => name.clone(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::ir::test_fixtures::connection_layout;

    #[test]
    fn explain_contains_field_names() {
        let layout = connection_layout();
        let out = render_explain(&layout);
        assert!(out.contains("timeout"));
        assert!(out.contains("port"));
        assert!(out.contains("is_active"));
        assert!(out.contains("is_tls"));
    }

    #[test]
    fn explain_shows_padding_rows() {
        let layout = connection_layout();
        let out = render_explain(&layout);
        assert!(out.contains("<padding>"));
    }

    #[test]
    fn explain_shows_struct_size() {
        let layout = connection_layout();
        let out = render_explain(&layout);
        assert!(out.contains("24 bytes"));
    }

    #[test]
    fn explain_shows_reorder_suggestion() {
        let layout = connection_layout();
        let out = render_explain(&layout);
        assert!(out.contains("reorder"));
        assert!(out.contains("→"));
    }

    #[test]
    fn explain_shows_impact_scale_line() {
        let layout = connection_layout();
        let out = render_explain(&layout);
        // Connection saves 8B → should show ~8 KB per 1K and ~8 MB per 1M
        assert!(out.contains("~8 KB extra per 1K instances"));
        assert!(out.contains("~8 MB per 1M instances"));
        assert!(out.contains("extra cache lines/1M"));
    }

    #[test]
    fn explain_no_impact_line_when_no_savings() {
        let layout = padlock_core::ir::test_fixtures::packed_layout();
        let out = render_explain(&layout);
        assert!(!out.contains("KB extra per 1K"));
        assert!(!out.contains("MB per 1M"));
    }

    #[test]
    fn explain_shows_cache_line_separator_when_struct_spans_multiple_lines() {
        use padlock_core::arch::X86_64_SYSV;
        use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
        // Build a struct that spans two 64-byte cache lines
        let big = StructLayout {
            name: "Big".to_string(),
            total_size: 128,
            align: 8,
            fields: vec![
                Field {
                    name: "a".to_string(),
                    ty: TypeInfo::Primitive {
                        name: "u8[60]".to_string(),
                        size: 60,
                        align: 1,
                    },
                    offset: 0,
                    size: 60,
                    align: 1,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "b".to_string(),
                    ty: TypeInfo::Primitive {
                        name: "u64".to_string(),
                        size: 8,
                        align: 8,
                    },
                    offset: 64,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
            uncertain_fields: Vec::new(),
        };
        let out = render_explain(&big);
        assert!(
            out.contains("cache line 1"),
            "must show cache line 1 separator: {out}"
        );
    }

    #[test]
    fn explain_shows_cl_column_header() {
        let layout = connection_layout();
        let out = render_explain(&layout);
        assert!(out.contains("CL"), "CL column header must appear");
    }

    #[test]
    fn explain_cl_column_shows_zero_for_small_struct() {
        // connection_layout is 24 bytes — all fields on cache line 0
        let layout = connection_layout();
        let out = render_explain(&layout);
        // Every data row should have │  0 │ (CL 0) and none should show │  1 │
        assert!(out.contains("│  0 │"), "all fields must be on cache line 0");
        assert!(
            !out.contains("│  1 │"),
            "no field should be on cache line 1"
        );
    }

    #[test]
    fn explain_cl_column_shows_nonzero_for_large_struct() {
        use padlock_core::arch::X86_64_SYSV;
        use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
        let big = StructLayout {
            name: "Big".to_string(),
            total_size: 128,
            align: 8,
            fields: vec![
                Field {
                    name: "a".to_string(),
                    ty: TypeInfo::Primitive {
                        name: "u8[64]".to_string(),
                        size: 64,
                        align: 1,
                    },
                    offset: 0,
                    size: 64,
                    align: 1,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
                Field {
                    name: "b".to_string(),
                    ty: TypeInfo::Primitive {
                        name: "u64".to_string(),
                        size: 8,
                        align: 8,
                    },
                    offset: 64,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Unknown,
                },
            ],
            source_file: None,
            source_line: None,
            arch: &X86_64_SYSV,
            is_packed: false,
            is_union: false,
            is_repr_rust: false,
            suppressed_findings: Vec::new(),
            uncertain_fields: Vec::new(),
        };
        let out = render_explain(&big);
        // field 'b' starts at offset 64 → cache line 1
        assert!(out.contains("│  1 │"), "field b must show CL 1");
    }

    #[test]
    fn explain_no_cache_line_separator_for_small_struct() {
        // Connection (24 bytes) fits in one cache line — no separator expected
        let layout = connection_layout();
        let out = render_explain(&layout);
        assert!(
            !out.contains("cache line 1"),
            "single-cache-line struct must not show separator"
        );
    }

    #[test]
    fn fmt_count_formats_correctly() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1K");
        assert_eq!(fmt_count(125_000), "125K");
        assert_eq!(fmt_count(1_000_000), "1M");
        assert_eq!(fmt_count(2_500_000), "2M");
    }
}
