// padlock-cli/src/perf_types.rs
//
// `--perf-types <file>`: measured hot fields from Linux perf's data-type
// profiling (`perf mem record …; perf report --stdio -s type,typeoff`, perf ≥
// 6.8). Each line attributes a share of memory-access samples to a struct
// member (`struct conn +40 (state)`); fields that receive samples are marked hot,
// so locality and false-sharing analysis use measured access patterns instead of
// guessing from type names.

use std::collections::HashMap;
use std::path::Path;

use padlock_core::ir::{AccessPattern, StructLayout};

/// `struct name` → [(byte offset, share of all samples in percent)].
fn parse(text: &str) -> HashMap<String, Vec<(usize, f64)>> {
    let mut out: HashMap<String, Vec<(usize, f64)>> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((pct, rest)) = line.split_once('%') else {
            continue;
        };
        let Ok(pct) = pct.trim().parse::<f64>() else {
            continue;
        };
        // The offset column repeats the type: `struct conn +40 (state)`.
        let Some(plus) = rest.rfind(" +") else {
            continue;
        };
        let after = &rest[plus + 2..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let Ok(offset) = digits.parse::<usize>() else {
            continue;
        };
        let ty = rest[..plus].trim();
        // `struct conn  struct conn` → the last `struct NAME` before the offset.
        let name = ty
            .rsplit(|c: char| c.is_whitespace())
            .next()
            .unwrap_or(ty)
            .to_string();
        if name.is_empty() || name == "struct" || name == "union" || name == "class" {
            continue;
        }
        out.entry(name).or_default().push((offset, pct));
    }
    out
}

/// Mark the fields that perf saw accessed as hot. Returns how many fields were marked.
pub fn apply(path: &Path, layouts: &mut [StructLayout]) -> anyhow::Result<usize> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    let samples = parse(&text);
    let mut marked = 0;
    for l in layouts.iter_mut() {
        let short = l.name.rsplit("::").next().unwrap_or(&l.name).to_string();
        let Some(hits) = samples.get(&l.name).or_else(|| samples.get(&short)) else {
            continue;
        };
        for f in l.fields.iter_mut() {
            let touched = hits.iter().any(|(off, pct)| {
                *pct > 0.0 && *off >= f.offset && *off < f.offset + f.size.max(1)
            });
            if touched && matches!(f.access, AccessPattern::Unknown) {
                f.access = AccessPattern::ReadMostly;
                marked += 1;
            }
        }
    }
    Ok(marked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_data_type_profile_lines() {
        let text = "# Overhead  Data Type  Data Type Offset\n\
                    #\n\
                    \x20   23.08%  struct conn  struct conn +40 (state)\n\
                    \x20    4.00%  struct conn  struct conn +0 (fd)\n\
                    \x20    1.00%  (unknown)  (unknown) +0 (no field)\n";
        let p = parse(text);
        assert_eq!(p["conn"], vec![(40, 23.08), (0, 4.0)]);
    }
}
