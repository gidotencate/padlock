//! Integer constant-expression evaluation shared by the source frontends, used
//! to size fixed-length arrays (`char b[N * 2 + 1]`, `[64 - unsafe.Sizeof(x)]byte`,
//! `[u8; N]`, `[N]u8`).
//!
//! The evaluator is language-agnostic: identifiers are looked up and `sizeof`
//! operands are sized through caller-supplied closures, so each frontend keeps
//! its own constant table and type-size logic.

/// Callbacks that give an expression its meaning.
pub struct Env<'a> {
    /// Defining expression text for a constant identifier (`N` → `"8"`).
    pub lookup: &'a dyn Fn(&str) -> Option<String>,
    /// Size in bytes of the type (or value) named by a `sizeof`-style operand.
    pub sizeof: &'a dyn Fn(&str) -> Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Ident(String),
    /// Operand text of a `sizeof`-like call.
    Sizeof(String),
    Op(&'static str),
}

/// Last path segment names that introduce a size query:
/// C `sizeof(T)`, Go `unsafe.Sizeof(x)`, Zig `@sizeOf(T)`, Rust `size_of::<T>()`.
fn is_sizeof_name(path: &str) -> bool {
    let last = path.rsplit(['.', ':']).next().unwrap_or(path);
    matches!(last, "sizeof" | "Sizeof" | "@sizeOf" | "size_of")
}

/// Text between the balanced `open`/`close` pair starting at `s[0]`, plus the
/// number of bytes consumed (including both delimiters).
fn balanced(s: &str, open: char, close: char) -> Option<(&str, usize)> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some((&s[open.len_utf8()..i], i + close.len_utf8()));
            }
        }
    }
    None
}

fn parse_number(lit: &str) -> Option<i64> {
    let lit: String = lit.chars().filter(|c| *c != '_').collect();
    // Type suffixes: C `u`/`l`, Rust `u8`/`usize`/`i64`…
    let mut lit = lit.as_str();
    if !lit.starts_with("0x") && !lit.starts_with("0X") {
        for suf in [
            "usize", "isize", "u128", "i128", "u64", "i64", "u32", "i32", "u16", "i16", "u8", "i8",
        ] {
            if let Some(rest) = lit.strip_suffix(suf) {
                lit = rest;
                break;
            }
        }
    }
    let lit = lit.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(h) = lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(b) = lit.strip_prefix("0b").or_else(|| lit.strip_prefix("0B")) {
        i64::from_str_radix(b, 2).ok()
    } else if let Some(o) = lit.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()
    } else if lit.len() > 1 && lit.starts_with('0') {
        i64::from_str_radix(&lit[1..], 8).ok()
    } else {
        lit.parse().ok()
    }
}

fn tokenize(expr: &str) -> Option<Vec<Tok>> {
    const OPS: [&str; 13] = [
        "<<", ">>", "+", "-", "*", "/", "%", "&", "|", "^", "~", "(", ")",
    ];
    let b = expr.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(Tok::Num(parse_number(&expr[start..i])?));
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'@' {
            // A (possibly qualified) name: `N`, `unsafe.Sizeof`, `std::mem::size_of`, `@sizeOf`.
            let start = i;
            i += 1;
            while i < b.len() {
                let ch = b[i];
                let qualified = (ch == b'.'
                    && expr[i + 1..].starts_with(|c: char| c.is_alphabetic()))
                    || (ch == b':'
                        && expr[i..].starts_with("::")
                        && expr[i + 2..].starts_with(|c: char| c.is_alphabetic() || c == '_'));
                if ch.is_ascii_alphanumeric() || ch == b'_' {
                    i += 1;
                } else if qualified {
                    i += if ch == b':' { 2 } else { 1 };
                } else {
                    break;
                }
            }
            let path = &expr[start..i];
            // `N as usize`: integer casts do not change the value we model.
            if path == "as" {
                let rest = expr[i..].trim_start();
                let ty_len = rest
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                if ty_len == 0 || toks.is_empty() {
                    return None;
                }
                i = expr.len() - rest.len() + ty_len;
                continue;
            }
            if is_sizeof_name(path) {
                let rest = &expr[i..];
                // Rust: `size_of::<T>()`
                if let Some(turbofish) = rest.trim_start().strip_prefix("::<") {
                    let (inner, used) = balanced(&format!("<{turbofish}"), '<', '>')
                        .map(|(t, n)| (t.to_string(), n))?;
                    let after = &turbofish[used - 1..];
                    let after = after.trim_start().strip_prefix("()")?;
                    toks.push(Tok::Sizeof(inner.trim().to_string()));
                    i = expr.len() - after.len();
                    continue;
                }
                let lead = rest.len() - rest.trim_start().len();
                let call = rest.trim_start();
                if !call.starts_with('(') {
                    return None;
                }
                let (inner, used) = balanced(call, '(', ')')?;
                toks.push(Tok::Sizeof(inner.trim().to_string()));
                i += lead + used;
            } else {
                toks.push(Tok::Ident(path.to_string()));
            }
        } else {
            let op = OPS.iter().find(|op| expr[i..].starts_with(**op))?;
            toks.push(Tok::Op(op));
            i += op.len();
        }
    }
    Some(toks)
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    env: &'a Env<'a>,
    depth: u8,
}

impl Parser<'_> {
    fn peek_op(&self) -> Option<&'static str> {
        match self.toks.get(self.pos) {
            Some(Tok::Op(op)) => Some(op),
            _ => None,
        }
    }

    fn prec(op: &str) -> Option<u8> {
        Some(match op {
            "|" => 1,
            "^" => 2,
            "&" => 3,
            "<<" | ">>" => 4,
            "+" | "-" => 5,
            "*" | "/" | "%" => 6,
            _ => return None,
        })
    }

    fn binary(&mut self, min_prec: u8) -> Option<i64> {
        let mut lhs = self.unary()?;
        while let Some(op) = self.peek_op() {
            let Some(p) = Self::prec(op) else { break };
            if p < min_prec {
                break;
            }
            self.pos += 1;
            let rhs = self.binary(p + 1)?;
            lhs = match op {
                "|" => lhs | rhs,
                "^" => lhs ^ rhs,
                "&" => lhs & rhs,
                "<<" => lhs.checked_shl(u32::try_from(rhs).ok()?)?,
                ">>" => lhs.checked_shr(u32::try_from(rhs).ok()?)?,
                "+" => lhs.checked_add(rhs)?,
                "-" => lhs.checked_sub(rhs)?,
                "*" => lhs.checked_mul(rhs)?,
                "/" => lhs.checked_div(rhs)?,
                "%" => lhs.checked_rem(rhs)?,
                _ => return None,
            };
        }
        Some(lhs)
    }

    fn unary(&mut self) -> Option<i64> {
        let tok = self.toks.get(self.pos)?.clone();
        self.pos += 1;
        match tok {
            Tok::Num(n) => Some(n),
            Tok::Op("-") => self.unary().map(|v| -v),
            Tok::Op("+") => self.unary(),
            Tok::Op("~") => self.unary().map(|v| !v),
            Tok::Op("(") => {
                let v = self.binary(1)?;
                (self.peek_op() == Some(")")).then(|| self.pos += 1)?;
                Some(v)
            }
            Tok::Sizeof(t) => (self.env.sizeof)(&t),
            Tok::Ident(id) => {
                if self.depth > 8 {
                    return None;
                }
                let text = (self.env.lookup)(&id)?;
                eval(&text, self.env, self.depth + 1)
            }
            Tok::Op(_) => None,
        }
    }
}

/// Evaluate an integer constant expression. Returns `None` for anything that is
/// not fully evaluable (unknown identifier, unsupported operator, overflow).
pub fn eval(expr: &str, env: &Env<'_>, depth: u8) -> Option<i64> {
    let mut p = Parser {
        toks: tokenize(expr)?,
        pos: 0,
        env,
        depth,
    };
    let v = p.binary(1)?;
    (p.pos == p.toks.len()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(expr: &str) -> Option<i64> {
        let lookup = |id: &str| match id {
            "N" => Some("8".to_string()),
            "M" => Some("N * 2".to_string()),
            "cpu.Pad" => Some("64".to_string()),
            _ => None,
        };
        let sizeof = |t: &str| match t {
            "long" | "u64" | "atomic.Int64{}" => Some(8),
            "u8" => Some(1),
            _ => None,
        };
        eval(
            expr,
            &Env {
                lookup: &lookup,
                sizeof: &sizeof,
            },
            0,
        )
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ev("N * 2 + 1"), Some(17));
        assert_eq!(ev("(N + 1) * 2"), Some(18));
        assert_eq!(ev("1 << 4 | 3"), Some(19));
        assert_eq!(ev("M - 1"), Some(15));
        assert_eq!(ev("-N + 10"), Some(2));
    }

    #[test]
    fn literals() {
        assert_eq!(ev("0x10"), Some(16));
        assert_eq!(ev("1_000"), Some(1000));
        assert_eq!(ev("64usize"), Some(64));
        assert_eq!(ev("10UL"), Some(10));
        assert_eq!(ev("0b101"), Some(5));
    }

    #[test]
    fn sizeof_forms() {
        assert_eq!(ev("sizeof(long) * 3"), Some(24));
        assert_eq!(ev("64 - unsafe.Sizeof(atomic.Int64{})"), Some(56));
        assert_eq!(ev("@sizeOf(u64)"), Some(8));
        assert_eq!(ev("std::mem::size_of::<u64>() * 2"), Some(16));
        assert_eq!(ev("size_of::<u8>()"), Some(1));
    }

    #[test]
    fn integer_casts_are_transparent() {
        assert_eq!(ev("N as usize * 2"), Some(16));
        assert_eq!(ev("(N + 1) as u32"), Some(9));
        assert_eq!(ev("as usize"), None);
    }

    #[test]
    fn qualified_identifier() {
        assert_eq!(ev("cpu.Pad - 16"), Some(48));
    }

    #[test]
    fn unevaluable_is_none() {
        assert_eq!(ev("UNKNOWN"), None);
        assert_eq!(ev("N / 0"), None);
        assert_eq!(ev("sizeof(struct X)"), None);
        assert_eq!(ev("N +"), None);
    }
}
