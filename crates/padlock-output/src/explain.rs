// padlock-output/src/explain.rs
//
// Renders a visual field-by-field memory layout table for a single struct.
// Shows each field's offset, size, alignment, and padding gaps inline.

use padlock_core::ir::{find_padding, StructLayout, TypeInfo};

/// Render a visual layout table for one struct.
///
/// Example output:
///
/// ```text
/// ReadyEvent  24 bytes  align=4
/// ┌────────────────────────────────────────────────────────────┐
/// │ offset │ size │ align │ field                              │
/// ├────────────────────────────────────────────────────────────┤
/// │      0 │    1 │     1 │ tick: u8                          │
/// │      1 │    3 │     — │ <padding>                         │
/// │      4 │    4 │     4 │ ready: Ready                      │
/// │      8 │    1 │     1 │ is_shutdown: bool                 │
/// │      9 │   15 │     — │ <padding> (trailing)              │
/// └────────────────────────────────────────────────────────────┘
/// 14 bytes wasted (58%) — reorder: ready, tick, is_shutdown → 8 bytes
/// ```
pub fn render_explain(layout: &StructLayout) -> String {
    use padlock_core::analysis::reorder;

    let mut out = String::new();

    // Header
    let loc = match (&layout.source_file, layout.source_line) {
        (Some(f), Some(l)) => format!("  ({}:{})", f, l),
        (Some(f), None) => format!("  ({})", f),
        _ => String::new(),
    };
    out.push_str(&format!(
        "{}{}\n",
        layout.name, loc
    ));
    out.push_str(&format!(
        "{} bytes  align={}  fields={}{}\n",
        layout.total_size,
        layout.align,
        layout.fields.len(),
        if layout.is_packed { "  [packed]" } else { "" },
    ));

    // Table
    let col_field = 36usize;
    let divider = format!("├{:─<8}┼{:─<6}┼{:─<7}┼{:─<col_field$}┤", "", "", "", "");
    let top     = format!("┌{:─<8}┬{:─<6}┬{:─<7}┬{:─<col_field$}┐", "", "", "", "");
    let bot     = format!("└{:─<8}┴{:─<6}┴{:─<7}┴{:─<col_field$}┘", "", "", "", "");
    let header  = format!("│ {:>6} │ {:>4} │ {:>5} │ {:<col_field$}│", "offset", "size", "align", "field");

    out.push_str(&top);
    out.push('\n');
    out.push_str(&header);
    out.push('\n');
    out.push_str(&divider);
    out.push('\n');

    // Build rows: interleave fields with padding gaps
    #[derive(Debug)]
    enum Row {
        Field { offset: usize, size: usize, align: usize, name: String, ty: String },
        Pad  { offset: usize, size: usize, trailing: bool },
    }

    let mut rows: Vec<Row> = Vec::new();
    let gaps = find_padding(layout);

    let last_field_name = layout.fields.last().map(|f| f.name.as_str()).unwrap_or("");

    // Index gaps by the field they follow (by after_field name).
    // find_padding includes trailing padding as a gap after the last field.
    for field in &layout.fields {
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

    for row in &rows {
        match row {
            Row::Field { offset, size, align, name, ty } => {
                let label = format!("{}: {}", name, ty);
                let label = if label.len() > col_field {
                    format!("{}…", &label[..col_field - 1])
                } else {
                    label
                };
                out.push_str(&format!(
                    "│ {:>6} │ {:>4} │ {:>5} │ {:<col_field$}│\n",
                    offset, size, align, label
                ));
            }
            Row::Pad { offset, size, trailing } => {
                let label = if *trailing {
                    "<padding> (trailing)".to_string()
                } else {
                    "<padding>".to_string()
                };
                out.push_str(&format!(
                    "│ {:>6} │ {:>4} │ {:>5} │ {:<col_field$}│\n",
                    offset, size, "—", label
                ));
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
                wasted, pct,
                opt_order.join(", "),
                opt_size
            ));
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
}
