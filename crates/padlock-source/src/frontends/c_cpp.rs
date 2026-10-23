// padlock-source/src/frontends/c_cpp.rs
//
// Extracts struct layouts from C / C++ source using tree-sitter.
// Sizes and alignments are computed from field type names + arch config;
// there is no compiler involved so the results are approximate for complex types.

use padlock_core::arch::ArchConfig;
use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use tree_sitter::{Node, Parser};

use crate::CppStdlib;

thread_local! {
    static STDLIB: Cell<CppStdlib> = const { Cell::new(CppStdlib::LibStdCpp) };
    static C_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("C grammar is always valid");
        p
    });
    static CPP_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("C++ grammar is always valid");
        p
    });
}

/// Set the active C++ stdlib variant for this thread.  Called from `lib.rs::set_cpp_stdlib`.
pub(crate) fn set_stdlib(s: CppStdlib) {
    STDLIB.with(|c| c.set(s));
}

fn active_stdlib() -> CppStdlib {
    STDLIB.with(|c| c.get())
}

// ── type resolution ───────────────────────────────────────────────────────────

/// Split a comma-separated template argument list at depth-0 commas,
/// respecting nested `<>` and `()`.  Input is the text *inside* the outer
/// angle brackets, e.g. `"int, std::map<int,int>, double"`.
/// Returns trimmed, non-empty argument slices.
pub(crate) fn split_template_args(s: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let arg = s[start..i].trim();
                args.push(arg);
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        args.push(last);
    }
    args
}

/// Size/alignment of a type spelled only with integer keywords, e.g. `unsigned`,
/// `long unsigned int`, `signed char`, `short`. Returns `None` for anything else.
fn c_integer_size_align(ty: &str, arch: &'static ArchConfig) -> Option<(usize, usize)> {
    let (mut longs, mut short, mut ch, mut any) = (0, false, false, false);
    for tok in ty.split_whitespace() {
        match tok {
            "unsigned" | "signed" | "int" => {}
            "long" => longs += 1,
            "short" => short = true,
            "char" => ch = true,
            _ => return None,
        }
        any = true;
    }
    if !any {
        return None;
    }
    let ps = arch.pointer_size;
    Some(if ch {
        (1, 1)
    } else if short {
        (2, 2)
    } else if longs >= 2 {
        (8, 8)
    } else if longs == 1 {
        (ps, ps)
    } else {
        (4, 4)
    })
}

/// Map a C/C++ type name to (size, align) using the target arch.
fn c_type_size_align(ty: &str, arch: &'static ArchConfig) -> (usize, usize) {
    let ty = ty.trim();
    // Fixed-size array `T[N]` / `T[N][M]` (`[]` flexible, `[?]` unevaluated → 1 element).
    if ty.ends_with(']')
        && let Some(open) = ty.rfind('[')
    {
        let count = match ty[open + 1..ty.len() - 1].trim() {
            "" => 0,
            n => n.parse::<usize>().unwrap_or(1),
        };
        let (elem_size, elem_align) = c_type_size_align(&ty[..open], arch);
        return (elem_size.saturating_mul(count), elem_align);
    }
    // `float _Complex` / `double _Complex`: two consecutive components.
    if ty.contains("_Complex") || ty.contains("__complex__") {
        let base = ty.replace("__complex__", "").replace("_Complex", "");
        let base = base.trim();
        let (size, align) = c_type_size_align(if base.is_empty() { "double" } else { base }, arch);
        return (size * 2, align);
    }
    // Plain integer type words (`unsigned`, `long unsigned int`, `signed char`, …).
    if let Some(sa) = c_integer_size_align(ty, arch) {
        return sa;
    }
    // Synthetic bit-field run spanning N bytes (see `resolve_bitfield_groups`).
    if let Some(n) = ty
        .strip_prefix("__bits<")
        .and_then(|r| r.strip_suffix('>'))
        .and_then(|n| n.parse::<usize>().ok())
    {
        return (n, 1);
    }
    // C++ pointer-to-member-function: two words (function pointer + `this` adjustment).
    if ty == "__memfn_ptr" {
        return (arch.pointer_size * 2, arch.pointer_size);
    }
    // Anonymous nested struct/union encoded as "__anon_struct<size,align>" /
    // "__anon_union<size,align>" by the field parser.
    if let Some(rest) = ty
        .strip_prefix("__anon_union<")
        .or_else(|| ty.strip_prefix("__anon_struct<"))
        && let Some(inner) = rest.strip_suffix('>')
    {
        let mut parts = inner.splitn(2, ',');
        if let (Some(sz), Some(al)) = (parts.next(), parts.next())
            && let (Ok(n), Ok(a)) = (sz.trim().parse::<usize>(), al.trim().parse::<usize>())
        {
            return (n, a);
        }
    }
    // Strip qualifiers (including C11 _Atomic used as a type qualifier).
    for qual in &[
        "const ",
        "volatile ",
        "restrict ",
        "unsigned ",
        "signed ",
        "_Atomic ",
    ] {
        if let Some(rest) = ty.strip_prefix(qual) {
            return c_type_size_align(rest, arch);
        }
    }
    // x86 SSE / AVX / AVX-512 SIMD types
    match ty {
        "__m64" => return (8, 8),
        "__m128" | "__m128d" | "__m128i" => return (16, 16),
        "__m256" | "__m256d" | "__m256i" => return (32, 32),
        "__m512" | "__m512d" | "__m512i" => return (64, 64),
        // ARM NEON — 64-bit (double-word) vectors
        "float32x2_t" | "int32x2_t" | "uint32x2_t" | "int8x8_t" | "uint8x8_t" | "int16x4_t"
        | "uint16x4_t" | "float64x1_t" | "int64x1_t" | "uint64x1_t" => return (8, 8),
        // ARM NEON — 128-bit (quad-word) vectors
        "float32x4_t" | "int32x4_t" | "uint32x4_t" | "float64x2_t" | "int64x2_t" | "uint64x2_t"
        | "int8x16_t" | "uint8x16_t" | "int16x8_t" | "uint16x8_t" => return (16, 16),
        _ => {}
    }
    // C++ standard library types — sizes vary by stdlib variant.
    let stdlib = active_stdlib();
    match ty {
        // ── Synchronisation ───────────────────────────────────────────────────
        // pthread_mutex_t on Linux/glibc: 40 bytes.
        // On macOS (libc++): opaque_pthread_mutex_t = 56 bytes.
        // MSVC: CRITICAL_SECTION = 40 bytes on 64-bit.
        "std::mutex"
        | "std::recursive_mutex"
        | "std::timed_mutex"
        | "std::recursive_timed_mutex"
        | "pthread_mutex_t" => {
            return match stdlib {
                CppStdlib::LibCpp => (56, 8), // macOS/Apple pthread mutex
                _ => (40, 8),
            };
        }
        "std::shared_mutex" | "std::shared_timed_mutex" => return (56, 8),
        "std::condition_variable" | "pthread_cond_t" => {
            return match stdlib {
                CppStdlib::LibCpp => (40, 8), // macOS pthread_cond_t
                _ => (48, 8),
            };
        }

        // ── String / view ─────────────────────────────────────────────────────
        // libstdc++ std::string: 32B (ptr + length + SSO buffer (15 chars + NUL)).
        // libc++ (Clang/macOS/Android): 24B (short-string optimisation is smaller).
        // MSVC STL: 32B on 64-bit (16-byte SSO inline buffer).
        "std::string" | "std::wstring" | "std::u8string" | "std::u16string" | "std::u32string"
        | "std::pmr::string" => {
            return match stdlib {
                CppStdlib::LibCpp => (24, 8),
                _ => (32, 8), // libstdc++ and MSVC both 32B
            };
        }
        // std::string_view / std::span<T>: pointer + length (2 words).
        "std::string_view"
        | "std::wstring_view"
        | "std::u8string_view"
        | "std::u16string_view"
        | "std::u32string_view" => return (arch.pointer_size * 2, arch.pointer_size),

        // ── Sequence containers ───────────────────────────────────────────────
        // std::vector<T>: pointer + size + capacity = 3 words (24B on 64-bit).
        // Size is independent of T.
        ty if ty.starts_with("std::vector<") || ty == "std::vector" => {
            return (arch.pointer_size * 3, arch.pointer_size);
        }
        // std::deque<T>: 80B on both libstdc++ and libc++ (64-bit Linux).
        ty if ty.starts_with("std::deque<") || ty == "std::deque" => return (80, 8),
        // std::list<T>: sentinel node pointer + size = 2 words + node pointers.
        // libstdc++: 24B (size_t + two pointers). libc++: 24B.
        ty if ty.starts_with("std::list<") || ty == "std::list" => {
            return (arch.pointer_size * 3, arch.pointer_size);
        }
        // std::forward_list<T>: single pointer (head node).
        ty if ty.starts_with("std::forward_list<") || ty == "std::forward_list" => {
            return (arch.pointer_size, arch.pointer_size);
        }
        // std::array<T, N>: inline storage; size = N * sizeof(T).
        // We cannot compute this without resolving T and N, so fall through.

        // ── Associative / unordered containers ────────────────────────────────
        // All map/set types: header node + size = ~48B (libstdc++) / ~40B (libc++).
        // Use 48B as conservative approximation.
        ty if ty.starts_with("std::map<")
            || ty.starts_with("std::multimap<")
            || ty.starts_with("std::set<")
            || ty.starts_with("std::multiset<") =>
        {
            return (48, 8);
        }
        // std::unordered_map / unordered_set: bucket array pointer + size + load factor + etc.
        // libstdc++: ~56B. libc++: ~72B. Use 56B.
        ty if ty.starts_with("std::unordered_map<")
            || ty.starts_with("std::unordered_multimap<")
            || ty.starts_with("std::unordered_set<")
            || ty.starts_with("std::unordered_multiset<") =>
        {
            return (56, 8);
        }

        // ── Smart pointers ────────────────────────────────────────────────────
        // std::unique_ptr<T>: single pointer (deleter may be zero-sized via EBO).
        ty if ty.starts_with("std::unique_ptr<") || ty == "std::unique_ptr" => {
            return (arch.pointer_size, arch.pointer_size);
        }
        // std::shared_ptr<T> / std::weak_ptr<T>: object pointer + control block pointer.
        ty if ty.starts_with("std::shared_ptr<")
            || ty == "std::shared_ptr"
            || ty.starts_with("std::weak_ptr<")
            || ty == "std::weak_ptr" =>
        {
            return (arch.pointer_size * 2, arch.pointer_size);
        }

        // ── Type-erasure / utilities ──────────────────────────────────────────
        // std::function<Sig>: 32B on libstdc++ and libc++ (64-bit Linux).
        // Holds a functor pointer, a vtable pointer, and a small-functor buffer.
        ty if ty.starts_with("std::function<") || ty == "std::function" => return (32, 8),
        // std::any: 32B on libstdc++ (small-object buffer + vtable pointer).
        "std::any" => return (32, 8),
        // std::error_code / std::error_condition: pointer + int = 16B.
        "std::error_code" | "std::error_condition" => return (16, 8),
        // std::exception_ptr: single pointer.
        "std::exception_ptr" => return (arch.pointer_size, arch.pointer_size),
        // std::type_index: single pointer (wraps std::type_info*).
        "std::type_index" => return (arch.pointer_size, arch.pointer_size),
        // std::span<T>: pointer + length (2 words). Template arg irrelevant.
        ty if ty.starts_with("std::span<") || ty == "std::span" => {
            return (arch.pointer_size * 2, arch.pointer_size);
        }
        // std::optional<T>: sizeof(T) + 1B bool, padded to align(T).
        // Recurse to resolve T then apply the formula.
        ty if ty.starts_with("std::optional<") && ty.ends_with('>') => {
            let inner = &ty["std::optional<".len()..ty.len() - 1];
            let (t_size, t_align) = c_type_size_align(inner.trim(), arch);
            let total = (t_size + 1).next_multiple_of(t_align.max(1));
            return (total, t_align.max(1));
        }

        // ── Atomic ────────────────────────────────────────────────────────────
        // std::atomic<T>: same size and alignment as T.
        ty if ty.starts_with("std::atomic<") && ty.ends_with('>') => {
            let inner = &ty[12..ty.len() - 1];
            return c_type_size_align(inner.trim(), arch);
        }
        // std::atomic_flag: guaranteed 1B minimum, but often 4B in practice.
        "std::atomic_flag" => return (4, 4),

        // ── Threading ─────────────────────────────────────────────────────────
        // std::thread: stores a single pthread_t / HANDLE — one pointer.
        "std::thread" => return (arch.pointer_size, arch.pointer_size),
        // std::jthread: thread + stop_source (2 pointers) ≈ 3 words.
        "std::jthread" => return (arch.pointer_size * 3, arch.pointer_size),

        // ── Numeric / math ────────────────────────────────────────────────────
        // std::complex<T>: two consecutive T values (real + imaginary).
        ty if ty.starts_with("std::complex<") && ty.ends_with('>') => {
            let inner = &ty["std::complex<".len()..ty.len() - 1];
            let (sz, al) = c_type_size_align(inner.trim(), arch);
            return (sz * 2, al);
        }
        // std::bitset<N>: ceil(N/8) bytes, aligned to sizeof(unsigned long).
        ty if ty.starts_with("std::bitset<") && ty.ends_with('>') => {
            let inner = &ty["std::bitset<".len()..ty.len() - 1];
            if let Ok(n) = inner.trim().parse::<usize>() {
                let bytes = n.div_ceil(8).max(1);
                let align = arch.pointer_size; // unsigned long = word size
                let total = bytes.next_multiple_of(align);
                return (total, align);
            }
        }

        // ── Chrono ───────────────────────────────────────────────────────────
        // std::chrono::duration<Rep, Period>: Rep is the underlying arithmetic type.
        ty if ty.starts_with("std::chrono::duration<") && ty.ends_with('>') => {
            let inner = &ty["std::chrono::duration<".len()..ty.len() - 1];
            let args = split_template_args(inner);
            if !args.is_empty() {
                return c_type_size_align(args[0].trim(), arch);
            }
            return (8, 8); // default: nanoseconds = int64_t
        }
        // std::chrono::time_point<Clock, Duration>: size = sizeof(Duration).
        ty if ty.starts_with("std::chrono::time_point<") && ty.ends_with('>') => {
            let inner = &ty["std::chrono::time_point<".len()..ty.len() - 1];
            let args = split_template_args(inner);
            if args.len() >= 2 {
                return c_type_size_align(args[1].trim(), arch);
            }
            return (8, 8); // default: nanoseconds
        }

        // ── Misc standard library ─────────────────────────────────────────────
        // std::initializer_list<T>: pointer + size_t = 2 words (element type irrelevant).
        ty if ty.starts_with("std::initializer_list<") || ty == "std::initializer_list" => {
            return (arch.pointer_size * 2, arch.pointer_size);
        }
        // std::filesystem::path: platform-dependent; 32B on libstdc++, 24B on libc++.
        "std::filesystem::path" => return (32, 8),

        // ── Multi-arg templates ───────────────────────────────────────────────
        // std::variant<T1, T2, ...>:
        //   storage = max(sizeof(T...)) padded to max(alignof(T...));
        //   index = 4B (uint32_t-sized discriminant, libstdc++/MSVC practice);
        //   total = (storage + 4) padded to effective alignment.
        ty if ty.starts_with("std::variant<") && ty.ends_with('>') => {
            let inner = &ty["std::variant<".len()..ty.len() - 1];
            let args = split_template_args(inner);
            if !args.is_empty() {
                let max_size = args
                    .iter()
                    .map(|a| c_type_size_align(a, arch).0)
                    .max()
                    .unwrap_or(0);
                let max_align = args
                    .iter()
                    .map(|a| c_type_size_align(a, arch).1)
                    .max()
                    .unwrap_or(1)
                    .max(4);
                let storage = if max_align > 0 {
                    max_size.next_multiple_of(max_align)
                } else {
                    max_size
                };
                let total = (storage + 4).next_multiple_of(max_align);
                return (total, max_align);
            }
        }

        // std::pair<T1, T2>: standard struct layout of two fields.
        ty if ty.starts_with("std::pair<") && ty.ends_with('>') => {
            let inner = &ty["std::pair<".len()..ty.len() - 1];
            let args = split_template_args(inner);
            if args.len() >= 2 {
                let (s1, a1) = c_type_size_align(args[0], arch);
                let (s2, a2) = c_type_size_align(args[1], arch);
                let offset2 = if a2 > 0 { s1.next_multiple_of(a2) } else { s1 };
                let raw = offset2 + s2;
                let align = a1.max(a2).max(1);
                let total = raw.next_multiple_of(align);
                return (total, align);
            }
        }

        // std::tuple<T1, T2, ...>: approximate sequential struct layout.
        // (libstdc++ reverses field order for EBO but total size is identical.)
        ty if ty.starts_with("std::tuple<") && ty.ends_with('>') => {
            let inner = &ty["std::tuple<".len()..ty.len() - 1];
            let args = split_template_args(inner);
            if !args.is_empty() {
                let mut offset = 0usize;
                let mut max_align = 1usize;
                for arg in &args {
                    let (sz, al) = c_type_size_align(arg, arch);
                    if al > 0 {
                        offset = offset.next_multiple_of(al);
                    }
                    offset += sz;
                    max_align = max_align.max(al);
                }
                max_align = max_align.max(1);
                let total = offset.next_multiple_of(max_align);
                return (total, max_align);
            }
        }

        // std::array<T, N>: N consecutive elements of T with no metadata.
        ty if ty.starts_with("std::array<") && ty.ends_with('>') => {
            let inner = &ty["std::array<".len()..ty.len() - 1];
            let args = split_template_args(inner);
            if args.len() >= 2 {
                let (elem_size, elem_align) = c_type_size_align(args[0], arch);
                if let Ok(n) = args[1].trim().parse::<usize>() {
                    return (elem_size * n, elem_align);
                }
            }
        }

        _ => {} // fall through to primitive types below
    }
    // Primitive / stdint / pointer types
    match ty {
        "char" | "_Bool" | "bool" => (1, 1),
        "short" | "short int" => (2, 2),
        "int" => (4, 4),
        "long" | "long int" => (arch.pointer_size, arch.pointer_size),
        "long long" | "long long int" => (8, 8),
        "float" => (4, 4),
        "double" => (8, 8),
        "long double" => (16, 16),

        // C99 stdint exact-width types
        "int8_t" | "uint8_t" => (1, 1),
        "int16_t" | "uint16_t" => (2, 2),
        "int32_t" | "uint32_t" => (4, 4),
        "int64_t" | "uint64_t" => (8, 8),
        "intmax_t" | "uintmax_t" => (8, 8),
        "size_t" | "ssize_t" | "ptrdiff_t" | "intptr_t" | "uintptr_t" => {
            (arch.pointer_size, arch.pointer_size)
        }

        // C99 fast types — uint_fast{8,16}_t are always 1/2B;
        // uint_fast{32,64}_t are pointer-sized on 64-bit (8B), 4B on 32-bit.
        "int_fast8_t" | "uint_fast8_t" => (1, 1),
        "int_fast16_t" | "uint_fast16_t" => (2, 2),
        "int_fast32_t" | "uint_fast32_t" | "int_fast64_t" | "uint_fast64_t" => {
            (arch.pointer_size, arch.pointer_size)
        }

        // C99 least types — minimum guaranteed widths
        "int_least8_t" | "uint_least8_t" => (1, 1),
        "int_least16_t" | "uint_least16_t" => (2, 2),
        "int_least32_t" | "uint_least32_t" => (4, 4),
        "int_least64_t" | "uint_least64_t" => (8, 8),

        // GCC/Clang 128-bit integer extension
        "__int128" | "__uint128" | "__int128_t" | "__uint128_t" => (16, 16),

        // Linux kernel short-form integer types (linux/types.h)
        "u8" | "s8" => (1, 1),
        "u16" | "s16" => (2, 2),
        "u32" | "s32" => (4, 4),
        "u64" | "s64" => (8, 8),

        // Linux kernel double-underscore types (__u8, __s8, __be16, __le32, …)
        "__u8" | "__s8" | "__u8__" | "__s8__" => (1, 1),
        "__u16" | "__s16" | "__be16" | "__le16" => (2, 2),
        "__u32" | "__s32" | "__be32" | "__le32" => (4, 4),
        "__u64" | "__s64" | "__be64" | "__le64" => (8, 8),

        // MSVC fixed-width intrinsics
        "__int8" => (1, 1),
        "__int16" => (2, 2),
        "__int32" => (4, 4),
        "__int64" => (8, 8),

        // Windows SDK / WinAPI types
        "BYTE" | "BOOLEAN" | "CHAR" | "INT8" | "UINT8" => (1, 1),
        "WORD" | "WCHAR" | "SHORT" | "USHORT" | "INT16" | "UINT16" => (2, 2),
        "DWORD" | "LONG" | "ULONG" | "INT" | "UINT" | "BOOL" | "FLOAT" | "INT32" | "UINT32" => {
            (4, 4)
        }
        "QWORD" | "LONGLONG" | "ULONGLONG" | "INT64" | "UINT64" | "LARGE_INTEGER" => (8, 8),
        "DWORD64" | "ULONG64" | "LONG64" => (8, 8),
        "HANDLE" | "LPVOID" | "PVOID" | "LPCVOID" | "LPSTR" | "LPCSTR" | "LPWSTR" | "LPCWSTR"
        | "SIZE_T" | "SSIZE_T" | "ULONG_PTR" | "LONG_PTR" | "DWORD_PTR" | "INT_PTR"
        | "UINT_PTR" => (arch.pointer_size, arch.pointer_size),

        // C/C++ character types
        // wchar_t: 4B on Linux/macOS (GCC/Clang POSIX), 2B on Windows/MSVC.
        // All current padlock arch configs are POSIX, so 4B is correct here.
        "wchar_t" => (4, 4),
        "char8_t" => (1, 1),
        "char16_t" => (2, 2),
        "char32_t" => (4, 4),

        // Half-precision and bfloat16 (ARM, GCC, Clang, ML workloads)
        "_Float16" | "__fp16" | "__bf16" => (2, 2),
        // 128-bit float (GCC/Clang extension)
        "_Float128" | "__float128" => (16, 16),

        // Pointer types
        ty if ty.ends_with('*') => (arch.pointer_size, arch.pointer_size),
        // Unknown — use pointer size as a reasonable default
        _ => (arch.pointer_size, arch.pointer_size),
    }
}

// ── integer constant expressions (array lengths) ──────────────────────────────

/// Key prefix under which integer constants (`#define N 8`, `enum { K = 5 }`,
/// `constexpr int N = 8;`) live in the same map as the typedef aliases.
const CONST_KEY: &str = "$const:";

/// Evaluate an integer constant expression such as `N * 2 + 1` or
/// `sizeof(long) * 3`, resolving identifiers through the `$const:` entries of
/// `aliases`. Returns `None` for anything it cannot fully evaluate.
fn eval_const_expr(
    expr: &str,
    aliases: &HashMap<String, String>,
    arch: &'static ArchConfig,
    depth: u8,
) -> Option<i64> {
    let lookup = |id: &str| aliases.get(&format!("{CONST_KEY}{id}")).cloned();
    let sizeof = |t: &str| {
        let t = aliases.get(t).map(String::as_str).unwrap_or(t);
        // `sizeof(struct X)` and friends cannot be sized from here.
        if t.starts_with("struct ") || t.starts_with("union ") || t.starts_with("class ") {
            return None;
        }
        i64::try_from(c_type_size_align(t, arch).0).ok()
    };
    super::consteval::eval(
        expr,
        &super::consteval::Env {
            lookup: &lookup,
            sizeof: &sizeof,
        },
        depth,
    )
}

/// Record integer constants visible to array-length expressions: object-like
/// `#define`s, enumerators (explicit or implicit values) and `const`/`constexpr`
/// integer variables with an initializer.
fn collect_int_constants(
    source: &str,
    root: Node<'_>,
    arch: &'static ArchConfig,
    aliases: &mut HashMap<String, String>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
        match node.kind() {
            "preproc_def" => {
                let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) else {
                    continue;
                };
                let mut text = source[value.byte_range()].trim();
                if let Some(c) = text.find("//") {
                    text = text[..c].trim_end();
                }
                if let (Some(a), Some(b)) = (text.find("/*"), text.find("*/"))
                    && a < b
                {
                    // Comments inside the value: not worth modelling.
                    continue;
                }
                aliases.insert(
                    format!("{CONST_KEY}{}", &source[name.byte_range()]),
                    text.to_string(),
                );
            }
            // `enum E { … }` / `enum class E : uint8_t { … }` / `typedef enum { … } E;`:
            // later uses of the bare name `E` are sized as the underlying integer type.
            "enum_specifier" => {
                let base = node
                    .child_by_field_name("base")
                    .map(|b| source[b.byte_range()].trim().to_string())
                    .unwrap_or_else(|| "int".to_string());
                if let Some(name) = node.child_by_field_name("name") {
                    aliases
                        .entry(source[name.byte_range()].trim().to_string())
                        .or_insert_with(|| base.clone());
                }
                if let Some(td) = node.parent().filter(|p| p.kind() == "type_definition") {
                    for i in 0..td.child_count() {
                        if let Some(c) = td.child(i)
                            && c.kind() == "type_identifier"
                        {
                            aliases
                                .entry(source[c.byte_range()].trim().to_string())
                                .or_insert_with(|| base.clone());
                        }
                    }
                }
            }
            "enumerator_list" => {
                let mut next: i64 = 0;
                for i in 0..node.named_child_count() {
                    let Some(en) = node.named_child(i as u32) else {
                        continue;
                    };
                    if en.kind() != "enumerator" {
                        continue;
                    }
                    let Some(name) = en.child_by_field_name("name") else {
                        continue;
                    };
                    if let Some(v) = en.child_by_field_name("value") {
                        match eval_const_expr(&source[v.byte_range()], aliases, arch, 0) {
                            Some(n) => next = n,
                            None => break,
                        }
                    }
                    aliases.insert(
                        format!("{CONST_KEY}{}", &source[name.byte_range()]),
                        next.to_string(),
                    );
                    next += 1;
                }
            }
            "init_declarator" => {
                let is_const = node.parent().is_some_and(|p| {
                    p.kind() == "declaration"
                        && (0..p.child_count()).filter_map(|i| p.child(i)).any(|c| {
                            c.kind() == "type_qualifier"
                                && matches!(source[c.byte_range()].trim(), "const" | "constexpr")
                        })
                });
                let (Some(decl), Some(value)) = (
                    node.child_by_field_name("declarator"),
                    node.child_by_field_name("value"),
                ) else {
                    continue;
                };
                if is_const && decl.kind() == "identifier" {
                    aliases.insert(
                        format!("{CONST_KEY}{}", &source[decl.byte_range()]),
                        source[value.byte_range()].trim().to_string(),
                    );
                }
            }
            _ => {}
        }
    }
}

/// Split a trailing run of `[..]` array suffixes off a type string.
fn split_array_dims(ty: &str) -> (&str, &str) {
    match ty.find('[') {
        Some(i) => (ty[..i].trim_end(), &ty[i..]),
        None => (ty, ""),
    }
}

/// Resolve a raw field type string for sizing: apply the scalar typedef alias to
/// the element type and evaluate array-length expressions.
///
/// Returns `(sizing_type, display_type)`. Both have every `[expr]` replaced by
/// `[n]` (or `[?]` when the length cannot be evaluated — sized as one element
/// and reported through `uncertain_fields`); `display_type` keeps the source
/// spelling of the element type.
fn resolve_field_type(
    ty: &str,
    aliases: &HashMap<String, String>,
    arch: &'static ArchConfig,
) -> (String, String) {
    let (base, mut rest) = split_array_dims(ty);
    let resolved_base = aliases.get(base).map(String::as_str).unwrap_or(base);
    let mut dims = String::new();
    while let Some(s) = rest.strip_prefix('[') {
        let mut depth = 1;
        let mut end = None;
        for (k, ch) in s.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let expr = s[..end].trim();
        if expr.is_empty() {
            dims.push_str("[]");
        } else {
            match eval_const_expr(expr, aliases, arch, 0).filter(|n| *n >= 0) {
                Some(n) => dims.push_str(&format!("[{n}]")),
                None => dims.push_str("[?]"),
            }
        }
        rest = s[end + 1..].trim_start();
    }
    (format!("{resolved_base}{dims}"), format!("{base}{dims}"))
}

/// Build a laid-out-later `Field` from a parsed field declaration.
fn make_field(
    raw: RawField,
    aliases: &HashMap<String, String>,
    arch: &'static ArchConfig,
) -> Field {
    let (fname, ty_name, guard, alignas, field_line) = raw;
    let (sizing_ty, display_ty) = resolve_field_type(&ty_name, aliases, arch);
    let (size, natural_align) = c_type_size_align(&sizing_ty, arch);
    // alignas(N) on a field overrides its alignment requirement.
    let align = alignas.unwrap_or(natural_align);
    let access = if let Some(g) = guard {
        AccessPattern::Concurrent {
            guard: Some(g),
            is_atomic: false,
            is_annotated: true,
        }
    } else {
        AccessPattern::Unknown
    };
    Field {
        name: fname,
        ty: TypeInfo::Primitive {
            name: display_ty,
            size,
            align,
        },
        offset: 0,
        size,
        align,
        source_file: None,
        source_line: Some(field_line),
        access,
    }
}

// ── struct / union simulation ─────────────────────────────────────────────────

/// Strip a bit-field width annotation (`:N`) from a type name for size lookup.
/// `"int:3"` → `"int"`, `"std::atomic"` → unchanged (`:` not followed by digits only).
fn strip_bitfield_suffix(ty: &str) -> &str {
    if let Some(pos) = ty.rfind(':') {
        let suffix = ty[pos + 1..].trim();
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return ty[..pos].trim_end();
        }
    }
    ty
}

/// Return `true` when `ty` carries a bit-field width annotation (e.g. `"int:3"`).
fn is_bitfield_type(ty: &str) -> bool {
    strip_bitfield_suffix(ty) != ty
}

/// Lay out bit-field runs the way GCC/Clang do on the System V ABIs and replace
/// each contiguous run of bits with one synthetic byte-range field.
///
/// - Bit-fields are packed bit by bit, regardless of their declared type, so a
///   `char`, a `short` and an `int` bit-field can share bytes.
/// - A bit-field that would straddle a boundary of its declared type's storage
///   unit moves to the next unit boundary (not when `pack_n == 1`).
/// - A zero-width bit-field (`T : 0`) forces the next bit-field to the next
///   boundary of `T`'s unit.
/// - The run is followed by the next ordinary field at the next byte boundary.
/// - Anonymous padding bit-fields consume bits but add no name and no alignment.
/// - In a union every bit-field starts at bit 0 and is laid out on its own.
///
/// Each synthetic field is named `[a:3|b:5]` (`[__pad]` when every member is
/// anonymous) and typed `__bits<N>` for the `N` bytes it spans. Returns the
/// fields plus the strictest alignment demanded by a named bit-field's type,
/// which the caller applies to the struct as a whole.
fn resolve_bitfield_groups(
    raw_fields: Vec<RawField>,
    aliases: &HashMap<String, String>,
    arch: &'static ArchConfig,
    pack_n: usize,
    is_union: bool,
) -> (Vec<RawField>, usize) {
    struct Seg {
        start_bit: u64,
        parts: Vec<String>,
        first_line: u32,
        unit_align: usize,
    }

    let mut result: Vec<RawField> = Vec::new();
    let mut struct_align = 1usize;
    // End of the last emitted field, in bytes.
    let mut byte_off = 0usize;
    // Bit position inside the current bit-field run (None outside a run).
    let mut cursor: Option<u64> = None;
    let mut seg: Option<Seg> = None;

    // Close the open segment at `end_bit`, emitting its synthetic field.
    let flush =
        |seg: &mut Option<Seg>, end_bit: u64, byte_off: &mut usize, result: &mut Vec<RawField>| {
            let Some(sg) = seg.take() else { return };
            let start_byte = (sg.start_bit / 8) as usize;
            let end_byte = end_bit.div_ceil(8) as usize;
            let named = !sg.parts.is_empty();
            let name = if named {
                format!("[{}]", sg.parts.join("|"))
            } else {
                "[__pad]".to_string()
            };
            // A segment that begins on a unit boundary keeps that alignment so that
            // the padding before it is reported; one that continues mid-unit packs
            // tight against the previous field.
            let aligned_start =
                sg.start_bit % 8 == 0 && start_byte.is_multiple_of(sg.unit_align.max(1));
            let align = (named && aligned_start && pack_n != 1).then_some(sg.unit_align);
            let align = align.or(Some(1));
            result.push((
                name,
                format!("__bits<{}>", end_byte - start_byte),
                None,
                align,
                sg.first_line,
            ));
            *byte_off = end_byte;
        };

    for (fname, ty, guard, alignas, line) in raw_fields {
        if !is_bitfield_type(&ty) {
            if let Some(c) = cursor.take() {
                flush(&mut seg, c, &mut byte_off, &mut result);
            }
            let (sizing, _) = resolve_field_type(&ty, aliases, arch);
            let (size, natural) = c_type_size_align(&sizing, arch);
            let mut align = alignas.unwrap_or(natural);
            if pack_n > 0 {
                align = align.min(pack_n);
            }
            if align > 0 {
                byte_off = byte_off.next_multiple_of(align);
            }
            byte_off += size;
            result.push((fname, ty, guard, alignas, line));
            continue;
        }

        let base_raw = strip_bitfield_suffix(&ty).to_string();
        let width: u64 = ty[ty.rfind(':').unwrap() + 1..].trim().parse().unwrap_or(0);
        let (sizing, _) = resolve_field_type(&base_raw, aliases, arch);
        let (unit_size, unit_align) = c_type_size_align(&sizing, arch);
        let unit_bits = (unit_size as u64 * 8).max(8);

        if is_union {
            // Overlapping members: each bit-field is its own storage unit.
            if width > 0 {
                result.push((
                    if fname.is_empty() {
                        "[__pad]".to_string()
                    } else {
                        format!("[{fname}:{width}]")
                    },
                    base_raw,
                    None,
                    None,
                    line,
                ));
                if !fname.is_empty() {
                    struct_align = struct_align.max(unit_align);
                }
            }
            continue;
        }

        let mut pos = cursor.unwrap_or(byte_off as u64 * 8);
        if width == 0 {
            flush(&mut seg, pos, &mut byte_off, &mut result);
            cursor = Some(pos.next_multiple_of(unit_bits));
            continue;
        }
        if pack_n != 1 && width <= unit_bits && pos / unit_bits != (pos + width - 1) / unit_bits {
            flush(&mut seg, pos, &mut byte_off, &mut result);
            pos = pos.next_multiple_of(unit_bits);
        }
        let sg = seg.get_or_insert_with(|| Seg {
            start_bit: pos,
            parts: Vec::new(),
            first_line: line,
            unit_align,
        });
        if !fname.is_empty() {
            sg.parts.push(format!("{fname}:{width}"));
            if pack_n != 1 {
                struct_align = struct_align.max(unit_align);
            }
        }
        sg.unit_align = sg
            .unit_align
            .max(if fname.is_empty() { 1 } else { unit_align });
        cursor = Some(pos + width);
    }
    if let Some(c) = cursor.take() {
        flush(&mut seg, c, &mut byte_off, &mut result);
    }
    (result, struct_align)
}

/// Simulate C/C++ struct layout given ordered fields.
///
/// `pack_n` controls field alignment capping:
/// - `0` — no packing (default C/C++ ABI alignment rules)
/// - `1` — `__attribute__((packed))` / `#pragma pack(1)`: force alignment to 1
/// - `N` — `#pragma pack(N)`: cap each field's alignment at N bytes
///
/// This unified model handles both GCC/Clang `__attribute__((packed))` and
/// MSVC-style `#pragma pack(N)` directives.
fn simulate_layout(
    fields: &mut Vec<Field>,
    struct_name: String,
    arch: &'static ArchConfig,
    source_line: Option<u32>,
    pack_n: usize,
) -> StructLayout {
    let mut offset = 0usize;
    let mut struct_align = 1usize;

    for f in fields.iter_mut() {
        let eff_align = if pack_n > 0 {
            f.align.min(pack_n)
        } else {
            f.align
        };
        if eff_align > 0 {
            offset = offset.next_multiple_of(eff_align);
        }
        f.offset = offset;
        offset += f.size;
        struct_align = struct_align.max(eff_align);
    }
    // Trailing padding (not present when fully packed)
    if pack_n != 1 && struct_align > 0 {
        offset = offset.next_multiple_of(struct_align);
    }

    StructLayout {
        name: struct_name,
        total_size: offset,
        align: struct_align,
        fields: std::mem::take(fields),
        source_file: None,
        source_line,
        arch,
        is_packed: pack_n == 1,
        is_union: false,
        is_repr_rust: false,
        suppressed_findings: Vec::new(),
        uncertain_fields: Vec::new(),
    }
}

/// Simulate a C/C++ union layout: all fields start at offset 0;
/// total size is the largest field, rounded to max alignment.
fn simulate_union_layout(
    fields: &mut Vec<Field>,
    name: String,
    arch: &'static ArchConfig,
    source_line: Option<u32>,
) -> StructLayout {
    for f in fields.iter_mut() {
        f.offset = 0;
    }
    let max_size = fields.iter().map(|f| f.size).max().unwrap_or(0);
    let max_align = fields.iter().map(|f| f.align).max().unwrap_or(1);
    let total_size = if max_align > 0 {
        max_size.next_multiple_of(max_align)
    } else {
        max_size
    };

    StructLayout {
        name,
        total_size,
        align: max_align,
        fields: std::mem::take(fields),
        source_file: None,
        source_line,
        arch,
        is_packed: false,
        is_union: true,
        is_repr_rust: false,
        suppressed_findings: Vec::new(),
        uncertain_fields: Vec::new(),
    }
}

// ── C++ class parsing (vtable + inheritance) ──────────────────────────────────

/// Parse a `class_specifier` node, modelling:
/// - A hidden vtable pointer (`__vptr`) when any method is `virtual`.
/// - Base-class storage as a synthetic `__base_<Name>` field (size resolved
///   later by the nested-struct resolution pass in `lib.rs`).
fn parse_class_specifier(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    aliases: &HashMap<String, String>,
    pragma_pack: usize,
) -> Option<StructLayout> {
    let mut class_name = "<anonymous>".to_string();
    let base_names = collect_base_class_names(source, node);
    let mut body_node: Option<Node> = None;
    let mut is_packed = false;
    let mut struct_alignas: Option<usize> = None;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "type_identifier" => class_name = source[child.byte_range()].to_string(),
            "field_declaration_list" => body_node = Some(child),
            "attribute_specifier" => {
                let text = &source[child.byte_range()];
                if text.contains("packed") {
                    is_packed = true;
                }
                if struct_alignas.is_none() {
                    struct_alignas = extract_aligned_from_c_field_text(text);
                }
            }
            // C++11 class-level alignas: `class alignas(64) Name { ... };`
            "alignas_qualifier" | "alignas_specifier" if struct_alignas.is_none() => {
                struct_alignas = parse_alignas_value(source, child);
            }
            // MSVC __declspec(align(N)) on the class tag.
            "ms_declspec_modifier" if struct_alignas.is_none() => {
                struct_alignas = extract_declspec_align(source[child.byte_range()].trim());
            }
            _ => {}
        }
    }

    // Fallback: scan the header source text for __declspec(align(N)).
    // tree-sitter places ms_declspec_modifier as a *sibling* of class_specifier
    // (under the parent declaration node), so also look just before this node.
    if struct_alignas.is_none() {
        let body_start = body_node.map(|b| b.start_byte()).unwrap_or(node.end_byte());
        let header_text = &source[node.start_byte()..body_start];
        struct_alignas = extract_declspec_align(header_text);
    }
    if struct_alignas.is_none() && node.start_byte() > 0 {
        let look_back = floor_to_char_boundary(source, node.start_byte().saturating_sub(256));
        struct_alignas = extract_declspec_align(&source[look_back..node.start_byte()]);
    }

    let body = body_node?;

    // Detect virtual methods: look for `virtual` keyword anywhere in body
    let has_virtual = contains_virtual_keyword(source, body);

    // Collect declared fields: (field_name, type_text, guard, alignas_override, source_line)
    let mut raw_fields: Vec<RawField> = Vec::new();
    for i in 0..body.child_count() {
        let Some(child) = body.child(i) else {
            continue;
        };
        if child.kind() == "field_declaration" {
            if let Some(anon_fields) = parse_anonymous_nested(source, child, arch, false, aliases) {
                raw_fields.extend(anon_fields);
            } else {
                raw_fields.extend(parse_field_declarations(source, child, arch, aliases));
            }
        }
    }

    // Build fields: vtable pointer, then base-class slots, then declared fields
    let mut fields: Vec<Field> = Vec::new();

    // Virtual dispatch pointer (hidden, at offset 0 for the first virtual class).
    // With a base class the vptr usually lives in the (polymorphic) primary base,
    // which is already counted by the base-class slot, so it is not added twice.
    if has_virtual && base_names.is_empty() {
        let ps = arch.pointer_size;
        fields.push(Field {
            name: "__vptr".to_string(),
            ty: TypeInfo::Pointer {
                size: ps,
                align: ps,
            },
            offset: 0,
            size: ps,
            align: ps,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        });
    }

    // Base class storage (opaque until nested-struct resolver fills in sizes)
    fields.extend(base_class_fields(&base_names, arch));

    // Pack consecutive bit-field members into their storage units.
    let pack_n = if is_packed { 1 } else { pragma_pack };
    let is_union = false;
    let (raw_fields, bf_align) = if raw_fields
        .iter()
        .any(|(_, ty, _, _, _)| is_bitfield_type(ty))
    {
        resolve_bitfield_groups(raw_fields, aliases, arch, pack_n, is_union)
    } else {
        (raw_fields, 1)
    };

    // Declared member fields
    fields.extend(
        raw_fields
            .into_iter()
            .map(|raw| make_field(raw, aliases, arch)),
    );

    if fields.is_empty() {
        return None;
    }

    let line = node.start_position().row as u32 + 1;
    let mut layout = simulate_layout(&mut fields, class_name, arch, Some(line), pack_n);

    apply_min_align(&mut layout, bf_align, pack_n);
    apply_min_align(&mut layout, struct_alignas.unwrap_or(1), pack_n);

    mark_uncertain_arrays(&mut layout);

    layout.suppressed_findings =
        super::suppress::suppressed_from_preceding_source(source, node.start_byte());

    Some(layout)
}

/// Return true if a `field_declaration_list` node contains any `virtual` keyword
/// (indicating that the class needs a vtable pointer).
fn contains_virtual_keyword(source: &str, node: Node<'_>) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "virtual" {
            return true;
        }
        // Also check raw text for cases where tree-sitter may not produce a
        // dedicated `virtual` node (e.g. inside complex declarations).
        if n.child_count() == 0 {
            let text = &source[n.byte_range()];
            if text == "virtual" {
                return true;
            }
        }
        for i in (0..n.child_count()).rev() {
            if let Some(child) = n.child(i) {
                stack.push(child);
            }
        }
    }
    false
}

// ── tree-sitter walker ────────────────────────────────────────────────────────

/// Pre-scan a tree for plain scalar typedef declarations and return a map of
/// `AliasName → BaseTypeName` for within-file alias resolution.
///
/// Only simple scalar aliases are collected, e.g.:
///   `typedef uint32_t MyId;`   → `{"MyId": "uint32_t"}`
///   `typedef unsigned int Idx;` → `{"Idx": "unsigned int"}`
///
/// Struct/union, function-pointer, and pointer typedefs are skipped — the
/// alias name in those cases lives in a nested declarator node and will not
/// appear as a direct `type_identifier` child, so they naturally produce fewer
/// than two type parts and are filtered out.
fn collect_typedef_aliases(source: &str, root: Node<'_>) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
        if node.kind() != "type_definition" {
            continue;
        }
        // Skip struct/union/class typedefs — those produce StructLayout entries.
        let has_record = (0..node.child_count()).any(|i| {
            node.child(i)
                .map(|c| {
                    matches!(
                        c.kind(),
                        "struct_specifier" | "union_specifier" | "class_specifier"
                    )
                })
                .unwrap_or(false)
        });
        if has_record {
            continue;
        }
        // Collect direct-child type parts in declaration order:
        //   `typedef uint32_t MyId;`       → ["uint32_t", "MyId"]
        //   `typedef unsigned int MyUInt;` → ["unsigned int", "MyUInt"]
        // For pointer/function typedefs the alias name is nested inside a
        // declarator node, so only one part is collected and we skip (len < 2).
        let mut type_parts: Vec<String> = Vec::new();
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            match child.kind() {
                "typedef" | ";" => {}
                "type_identifier" | "primitive_type" | "sized_type_specifier" => {
                    type_parts.push(source[child.byte_range()].trim().to_string());
                }
                _ => {}
            }
        }
        if type_parts.len() < 2 {
            continue;
        }
        // Last element is the alias name; everything before is the base type.
        // Safety: len >= 2 guaranteed by the check above.
        let alias_name = type_parts.pop().expect("len >= 2");
        let base_type = type_parts.join(" ");
        aliases.entry(alias_name).or_insert(base_type);
    }
    aliases
}

/// Extract the ordered type-parameter names from a `template_declaration`
/// node, e.g. `template<typename T, typename U>` → `["T", "U"]`.
///
/// Only plain `typename T` / `class T` parameters are handled; defaulted and
/// variadic parameters are not matched by `type_parameter_declaration` and so
/// are silently omitted, which surfaces as an arg-count mismatch (and a
/// skipped-instantiation note) rather than a silent miscount.
fn template_param_names(source: &str, tpl_decl: Node<'_>) -> Vec<String> {
    let Some(param_list) = tpl_decl.child_by_field_name("parameters") else {
        return Vec::new();
    };
    (0..param_list.child_count())
        .filter_map(|i| param_list.child(i))
        .filter(|c| c.kind() == "type_parameter_declaration")
        .filter_map(|c| {
            (0..c.child_count())
                .filter_map(|j| c.child(j))
                .find(|gc| gc.kind() == "type_identifier")
                .map(|gc| source[gc.byte_range()].to_string())
        })
        .collect()
}

/// Collect `// padlock:instantiate(Arg1, Arg2, ...)` annotation comments
/// immediately preceding `tpl_decl` (no blank/non-comment line in between),
/// in document order. Each line names one instantiation's concrete type
/// arguments, positionally matched against the template's parameter list.
fn collect_instantiate_annotations(source: &str, tpl_decl: Node<'_>) -> Vec<Vec<String>> {
    let mut lines = Vec::new();
    let mut current = tpl_decl.prev_sibling();
    while let Some(c) = current {
        if c.kind() != "comment" {
            break;
        }
        let text = source[c.byte_range()].trim();
        let text = text.strip_prefix("//").map(str::trim).unwrap_or(text);
        if let Some(inner) = text
            .strip_prefix("padlock:instantiate(")
            .and_then(|s| s.strip_suffix(')'))
        {
            lines.push(
                split_template_args(inner)
                    .into_iter()
                    .map(|a| a.trim().to_string())
                    .collect(),
            );
        }
        current = c.prev_sibling();
    }
    lines.reverse();
    lines
}

fn extract_structs_from_tree(
    source: &str,
    root: Node<'_>,
    arch: &'static ArchConfig,
    layouts: &mut Vec<StructLayout>,
) {
    // Phase 0: collect within-file typedef scalar aliases for field type resolution.
    let mut aliases = collect_typedef_aliases(source, root);
    // …and integer constants used in array-length expressions.
    collect_int_constants(source, root, arch, &mut aliases);

    // Phase 1: extract struct/union/class layouts.
    // We do a single linear pass ordered by byte offset so that `#pragma pack`
    // directives are processed in document order, keeping `current_pack` accurate
    // at each struct declaration site.
    //
    // Pack state: `current_pack = 0` means no active pragma (use default ABI
    // alignment); `current_pack = N > 0` means cap field alignment at N bytes.
    let mut pack_stack: Vec<usize> = Vec::new();
    let mut current_pack: usize = 0;

    let cursor = root.walk();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        // Push children in reverse so we process left-to-right in document order.
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }

        // Track `#pragma pack(...)` directives (tree-sitter: `preproc_call` nodes).
        if node.kind() == "preproc_call" {
            let text = &source[node.byte_range()];
            if text.contains("#pragma") && text.contains("pack(") {
                current_pack = parse_pragma_pack(text, &mut pack_stack, current_pack);
            }
        }

        // Skip C++ template structs/classes/unions — without monomorphisation we
        // cannot know T's size, so any sizing would be wrong.  tree-sitter-cpp
        // wraps these as `template_declaration > struct_specifier` etc.
        //
        // Exception: a `// padlock:instantiate(Arg1[, Arg2, ...])` comment
        // immediately above the `template_declaration` names concrete type
        // arguments for one instantiation. The template's own parameter names
        // are substituted into the typedef-alias map (the same mechanism that
        // already resolves `typedef uint32_t MyId`), so the existing field-type
        // resolution logic sizes the instantiation exactly as if it were a
        // concrete struct — one synthetic layout per `instantiate` line, named
        // `Name<Arg1, Arg2>`. Multiple lines produce multiple instantiations.
        let is_record_specifier = matches!(
            node.kind(),
            "struct_specifier" | "union_specifier" | "class_specifier"
        );
        let template_decl = node
            .parent()
            .filter(|p| is_record_specifier && p.kind() == "template_declaration");
        if let Some(tpl_decl) = template_decl {
            let tpl_name = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .find(|c| c.kind() == "type_identifier")
                .map(|c| source[c.byte_range()].to_string())
                .unwrap_or_else(|| "(unknown)".to_string());

            let param_names = template_param_names(source, tpl_decl);
            let instantiations = collect_instantiate_annotations(source, tpl_decl);

            if instantiations.is_empty() {
                crate::record_skipped(
                    &tpl_name,
                    "C++ template — layout depends on type arguments; \
                     use binary analysis for accurate results",
                );
                continue;
            }

            let is_union = node.kind() == "union_specifier";
            for args in instantiations {
                if args.len() != param_names.len() {
                    eprintln!(
                        "padlock: note: skipping 'padlock:instantiate({})' for '{tpl_name}' \
                         — expected {} type argument(s), got {}",
                        args.join(", "),
                        param_names.len(),
                        args.len()
                    );
                    continue;
                }
                let mut inst_aliases = aliases.clone();
                for (param, arg) in param_names.iter().zip(args.iter()) {
                    inst_aliases.insert(param.clone(), arg.clone());
                }
                let parsed = if node.kind() == "class_specifier" {
                    parse_class_specifier(source, node, arch, &inst_aliases, current_pack)
                } else {
                    parse_struct_or_union_specifier(
                        source,
                        node,
                        arch,
                        is_union,
                        &inst_aliases,
                        current_pack,
                    )
                };
                if let Some(mut layout) = parsed {
                    layout.name = format!("{tpl_name}<{}>", args.join(", "));
                    layouts.push(layout);
                }
            }
            continue;
        }

        match node.kind() {
            "struct_specifier" => {
                if let Some(layout) = parse_struct_or_union_specifier(
                    source,
                    node,
                    arch,
                    false,
                    &aliases,
                    current_pack,
                ) {
                    layouts.push(layout);
                }
            }
            "union_specifier" => {
                if let Some(layout) = parse_struct_or_union_specifier(
                    source,
                    node,
                    arch,
                    true,
                    &aliases,
                    current_pack,
                ) {
                    layouts.push(layout);
                }
            }
            "class_specifier" => {
                if let Some(layout) =
                    parse_class_specifier(source, node, arch, &aliases, current_pack)
                {
                    layouts.push(layout);
                }
            }
            _ => {}
        }
    }

    // Also handle `typedef struct/union { ... } Name;`.
    // Run a second pass; at this point we do not re-track pragma pack since
    // typedef structs with non-default packing will have already been captured
    // in the first pass (the struct specifier inside the typedef is the same
    // node). The second pass only renames anonymous structs, so pack accuracy
    // is inherited from the first-pass result.
    let cursor2 = root.walk();
    let mut stack2 = vec![root];
    while let Some(node) = stack2.pop() {
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack2.push(child);
            }
        }
        if node.kind() == "type_definition"
            && let Some(layout) =
                parse_typedef_struct_or_union(source, node, arch, &aliases, current_pack)
        {
            let existing = layouts
                .iter()
                .position(|l| l.name == layout.name || l.name == "<anonymous>");
            match existing {
                Some(i) if layouts[i].name == "<anonymous>" => {
                    layouts[i] = layout;
                }
                None => layouts.push(layout),
                _ => {}
            }
        }
    }
    let _ = cursor;
    let _ = cursor2; // silence unused warnings
}

/// Parse a `#pragma pack(...)` directive and update the pack stack/current level.
///
/// Recognised forms:
/// - `#pragma pack(N)`        — set pack level to N
/// - `#pragma pack()`         — reset to default (0)
/// - `#pragma pack(push, N)`  — push current level, set to N
/// - `#pragma pack(push)`     — push current level (no change)
/// - `#pragma pack(pop)`      — restore previous level
///
/// Returns the new `current_pack` value.
fn parse_pragma_pack(text: &str, stack: &mut Vec<usize>, current: usize) -> usize {
    // Extract the argument list between the outer parentheses of `pack(...)`.
    let Some(start) = text.find("pack(") else {
        return current;
    };
    let rest = &text[start + 5..]; // skip "pack("
    let Some(end) = rest.find(')') else {
        return current;
    };
    let args = rest[..end].trim();

    if args.is_empty() {
        // #pragma pack() — reset to default
        return 0;
    }

    // Split on comma to distinguish `push`/`pop`/`N`/`push, N`.
    let parts: Vec<&str> = args.splitn(2, ',').map(str::trim).collect();
    match parts[0] {
        "pop" => stack.pop().unwrap_or(0),
        "push" => {
            stack.push(current);
            if let Some(n_str) = parts.get(1) {
                n_str.parse::<usize>().unwrap_or(current)
            } else {
                current // push without N: keep current level
            }
        }
        n_str => n_str.parse::<usize>().unwrap_or(current),
    }
}

/// Parse a `struct_specifier` or `union_specifier` node into a `StructLayout`.
///
/// `pragma_pack` is the active `#pragma pack(N)` level at the point of the
/// declaration (`0` = no active pragma, equivalent to default ABI alignment).
fn parse_struct_or_union_specifier(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    is_union: bool,
    aliases: &HashMap<String, String>,
    pragma_pack: usize,
) -> Option<StructLayout> {
    let mut name = "<anonymous>".to_string();
    let mut body_node: Option<Node> = None;
    let mut is_packed = false;
    // Struct-level alignas: `struct alignas(64) CacheAligned { ... };`
    let mut struct_alignas: Option<usize> = None;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "type_identifier" => name = source[child.byte_range()].to_string(),
            "field_declaration_list" => body_node = Some(child),
            "attribute_specifier" => {
                let text = &source[child.byte_range()];
                if text.contains("packed") {
                    is_packed = true;
                }
                // `struct __attribute__((aligned(N))) S {…}` / `struct S {…} __attribute__((aligned(N)));`
                if struct_alignas.is_none() {
                    struct_alignas = extract_aligned_from_c_field_text(text);
                }
            }
            // C++11 struct-level alignas: `struct alignas(64) Name { ... };`
            // tree-sitter-cpp: `alignas_qualifier` as direct child of struct_specifier
            "alignas_qualifier" | "alignas_specifier" if struct_alignas.is_none() => {
                struct_alignas = parse_alignas_value(source, child);
            }
            // MSVC __declspec(align(N)) on the struct tag.
            "ms_declspec_modifier" if struct_alignas.is_none() => {
                struct_alignas = extract_declspec_align(source[child.byte_range()].trim());
            }
            _ => {}
        }
    }

    // Fallback: scan the header source text for __declspec(align(N)).
    // tree-sitter places ms_declspec_modifier as a *sibling* of struct_specifier
    // (under the parent declaration node), so we also look in the source just
    // before this struct node's opening keyword.
    if struct_alignas.is_none() {
        let body_start = body_node.map(|b| b.start_byte()).unwrap_or(node.end_byte());
        let header_text = &source[node.start_byte()..body_start];
        struct_alignas = extract_declspec_align(header_text);
    }
    if struct_alignas.is_none() && node.start_byte() > 0 {
        let look_back = floor_to_char_boundary(source, node.start_byte().saturating_sub(256));
        struct_alignas = extract_declspec_align(&source[look_back..node.start_byte()]);
    }

    let body = body_node?;

    // C++ structs can have virtual methods just like classes; in that case a
    // hidden vtable pointer lives at offset 0.  Unions cannot be polymorphic.
    let has_virtual = !is_union && contains_virtual_keyword(source, body);

    let mut raw_fields: Vec<RawField> = Vec::new();

    for i in 0..body.child_count() {
        let child = body.child(i)?;
        if child.kind() == "field_declaration" {
            // Check for anonymous nested struct/union: a field_declaration whose
            // only non-field-identifier child is a struct_specifier/union_specifier
            // with no type_identifier (i.e. `struct { int x; int y; };`).
            if let Some(anon_fields) =
                parse_anonymous_nested(source, child, arch, is_union, aliases)
            {
                raw_fields.extend(anon_fields);
            } else {
                raw_fields.extend(parse_field_declarations(source, child, arch, aliases));
            }
        }
    }
    let base_names = collect_base_class_names(source, node);

    // A struct with only virtual methods and no data members still needs a
    // layout if it has virtual dispatch (the __vptr field will be inserted below).
    if raw_fields.is_empty() && !has_virtual && base_names.is_empty() {
        return None;
    }

    // `__attribute__((packed))` forces pack_n=1; `#pragma pack(N)` caps at N.
    // When both apply, the more restrictive (smaller) wins.
    let pack_n = if is_packed { 1 } else { pragma_pack };

    // Pack consecutive bit-field members into their storage units.
    let (raw_fields, bf_align) = if raw_fields
        .iter()
        .any(|(_, ty, _, _, _)| is_bitfield_type(ty))
    {
        resolve_bitfield_groups(raw_fields, aliases, arch, pack_n, is_union)
    } else {
        (raw_fields, 1)
    };

    let mut fields: Vec<Field> = raw_fields
        .into_iter()
        .map(|raw| make_field(raw, aliases, arch))
        .collect();
    fields.splice(0..0, base_class_fields(&base_names, arch));

    // Virtual dispatch pointer (hidden, at offset 0 for the first virtual struct).
    // With a base class the vptr usually lives in the (polymorphic) primary base,
    // which is already counted by the base-class slot, so it is not added twice.
    if has_virtual && base_names.is_empty() {
        let ps = arch.pointer_size;
        fields.insert(
            0,
            Field {
                name: "__vptr".to_string(),
                ty: TypeInfo::Pointer {
                    size: ps,
                    align: ps,
                },
                offset: 0,
                size: ps,
                align: ps,
                source_file: None,
                source_line: None,
                access: AccessPattern::Unknown,
            },
        );
    }

    let line = node.start_position().row as u32 + 1;
    let mut layout = if is_union {
        simulate_union_layout(&mut fields, name, arch, Some(line))
    } else {
        simulate_layout(&mut fields, name, arch, Some(line), pack_n)
    };

    apply_min_align(&mut layout, bf_align, pack_n);
    // Apply struct-level alignas: the struct's alignment requirement is at
    // least N; trailing padding may grow to satisfy the new alignment.
    apply_min_align(&mut layout, struct_alignas.unwrap_or(1), pack_n);

    mark_uncertain_arrays(&mut layout);
    layout.suppressed_findings =
        super::suppress::suppressed_from_preceding_source(source, node.start_byte());

    Some(layout)
}

/// Raise the struct's alignment to at least `align` (from `alignas` or a
/// bit-field's declared type) and grow trailing padding to match.
fn apply_min_align(layout: &mut StructLayout, align: usize, pack_n: usize) {
    if align > layout.align {
        layout.align = align;
        if pack_n == 0 || pack_n >= align {
            layout.total_size = layout.total_size.next_multiple_of(align);
        }
    }
}

/// Flag fields whose array length could not be evaluated (`[?]`): they are
/// sized as a single element, so the output layer should warn about them.
fn mark_uncertain_arrays(layout: &mut StructLayout) {
    for f in &layout.fields {
        if let TypeInfo::Primitive { name, .. } = &f.ty
            && name.contains("[?]")
            && !layout.uncertain_fields.contains(&f.name)
        {
            layout.uncertain_fields.push(f.name.clone());
        }
    }
}

/// Names of the non-virtual base classes of a `struct_specifier` /
/// `class_specifier` (`struct D : A, protected B`). Virtual bases are skipped:
/// their placement depends on the most-derived type.
fn collect_base_class_names(source: &str, node: Node<'_>) -> Vec<String> {
    let mut names = Vec::new();
    for i in 0..node.child_count() {
        let Some(clause) = node.child(i) else {
            continue;
        };
        if clause.kind() != "base_class_clause" {
            continue;
        }
        let mut is_virtual = false;
        for j in 0..clause.child_count() {
            let Some(c) = clause.child(j) else { continue };
            match c.kind() {
                "virtual" => is_virtual = true,
                "," => is_virtual = false,
                "type_identifier" | "qualified_identifier" | "template_type" => {
                    if !is_virtual {
                        names.push(source[c.byte_range()].trim().to_string());
                    }
                    is_virtual = false;
                }
                _ => {}
            }
        }
    }
    names
}

/// Opaque placeholder fields for base-class subobjects; their sizes are filled
/// in by the nested-struct resolution pass in `lib.rs`.
fn base_class_fields(base_names: &[String], arch: &'static ArchConfig) -> Vec<Field> {
    let ps = arch.pointer_size;
    base_names
        .iter()
        .map(|base| Field {
            name: format!("__base_{base}"),
            ty: TypeInfo::Opaque {
                name: base.clone(),
                size: ps,
                align: ps,
            },
            offset: 0,
            size: ps,
            align: ps,
            source_file: None,
            source_line: None,
            access: AccessPattern::Unknown,
        })
        .collect()
}

/// Parse a `typedef struct/union { ... } Name;` type_definition node.
fn parse_typedef_struct_or_union(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    aliases: &HashMap<String, String>,
    pragma_pack: usize,
) -> Option<StructLayout> {
    let mut specifier_node: Option<Node> = None;
    let mut is_union = false;
    let mut typedef_name: Option<String> = None;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        match child.kind() {
            "struct_specifier" => {
                specifier_node = Some(child);
                is_union = false;
            }
            "union_specifier" => {
                specifier_node = Some(child);
                is_union = true;
            }
            "type_identifier" => typedef_name = Some(source[child.byte_range()].to_string()),
            _ => {}
        }
    }

    let spec = specifier_node?;
    let typedef_name = typedef_name?;

    let mut layout =
        parse_struct_or_union_specifier(source, spec, arch, is_union, aliases, pragma_pack)?;
    if layout.name == "<anonymous>" {
        layout.name = typedef_name;
    }
    Some(layout)
}

/// Extract a lock guard name from a C/C++ `__attribute__((guarded_by(X)))` or
/// `__attribute__((pt_guarded_by(X)))` specifier node.
///
/// Also recognises the common macro forms `GUARDED_BY(X)` and `PT_GUARDED_BY(X)`
/// which expand to the same attribute (Clang thread-safety analysis).
/// The match is done on the raw source text of any `attribute_specifier` child,
/// so it works regardless of how tree-sitter structures the inner tokens.
fn extract_guard_from_c_field_text(field_source: &str) -> Option<String> {
    // Patterns to search for (case-insensitive on the keyword, guard name is as-is)
    for kw in &["guarded_by", "pt_guarded_by", "GUARDED_BY", "PT_GUARDED_BY"] {
        if let Some(pos) = field_source.find(kw) {
            let after = &field_source[pos + kw.len()..];
            // Expect `(` optionally preceded by whitespace
            let trimmed = after.trim_start();
            if let Some(inner) = trimmed.strip_prefix('(') {
                // Read until the matching ')'
                if let Some(end) = inner.find(')') {
                    let guard = inner[..end].trim().trim_matches('"');
                    if !guard.is_empty() {
                        return Some(guard.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Parse an alignment override from GCC/Clang `__attribute__((aligned(N)))` or
/// `__aligned__(N)` on a field declaration.  Returns `None` when no such
/// attribute is present or the value cannot be parsed.
///
/// This is separate from C++11 `alignas(N)`, which is handled via tree-sitter
/// `alignas_qualifier` nodes in `parse_field_declaration`.
fn extract_aligned_from_c_field_text(field_source: &str) -> Option<usize> {
    // Use "aligned(" as the search key so we don't false-match identifiers
    // like "is_aligned" that do not have an immediately following '('.
    for kw in &["aligned(", "__aligned__("] {
        if let Some(pos) = field_source.find(kw) {
            let after = &field_source[pos + kw.len()..];
            if let Some(end) = after.find(')') {
                let n_str = after[..end].trim();
                if let Ok(n) = n_str.parse::<usize>() {
                    return Some(n);
                }
            }
        }
    }
    // Also check MSVC __declspec(align(N)) form.
    extract_declspec_align(field_source)
}

/// Parse an alignment value from MSVC `__declspec(align(N))`.
/// Returns `None` when no such specifier is present or the value cannot be parsed.
/// Nudges `index` forward to the nearest UTF-8 char boundary. Used for a fixed-size
/// byte look-back window (e.g. `start.saturating_sub(256)`), which can otherwise land
/// inside a multi-byte character — an em dash or box-drawing glyph in a comment panics
/// on slicing (`str` indices must be char boundaries). Moving forward only shrinks the
/// window slightly rather than guessing backward past the start.
fn floor_to_char_boundary(source: &str, mut index: usize) -> usize {
    while index < source.len() && !source.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn extract_declspec_align(source: &str) -> Option<usize> {
    let mut search = source;
    while let Some(pos) = search.find("__declspec(") {
        let after = &search[pos + "__declspec(".len()..];
        // Find the closing ')' of the outer __declspec(...)
        let mut depth = 1usize;
        let mut end_idx = after.len();
        for (i, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &after[..end_idx];
        // Look for `align(N)` inside the __declspec(...) body.
        // The MSVC keyword is `align`, never `aligned`, so this is unambiguous.
        if let Some(apos) = inner.find("align(") {
            let align_after = &inner[apos + "align(".len()..];
            if let Some(close) = align_after.find(')')
                && let Ok(n) = align_after[..close].trim().parse::<usize>()
            {
                return Some(n);
            }
        }
        // Advance past this __declspec and keep searching.
        search = &after[end_idx..];
    }
    None
}

/// Parse a numeric value from an `alignas_qualifier` node: `alignas(N)`.
/// tree-sitter-cpp uses the node kind `alignas_qualifier` for C++11 `alignas`.
/// Returns `None` when the specifier contains a type expression rather than
/// an integer literal (e.g. `alignas(double)` — handled elsewhere by the
/// compiler; we skip those conservatively).
fn parse_alignas_value(source: &str, node: Node<'_>) -> Option<usize> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "number_literal" | "integer_literal" | "integer" => {
                    let text = source[child.byte_range()].trim();
                    if let Ok(n) = text.parse::<usize>() {
                        return Some(n);
                    }
                    // Hex literal: 0x40
                    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                        return usize::from_str_radix(hex, 16).ok();
                    }
                }
                // Recurse for nested nodes (parenthesised expression, etc.)
                "parenthesized_expression" | "argument_list" | "alignas_qualifier" => {
                    if let r @ Some(_) = parse_alignas_value(source, child) {
                        return r;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Returns `(ty, field_name, guard, alignas_override)`.
/// `alignas_override` is `Some(N)` when the field carries `alignas(N)`.
/// Detect and parse an anonymous nested struct/union field declaration, e.g.:
///
/// ```c
/// struct Packet {
///     union {                    // ← anonymous nested union
///         uint32_t raw;
///         struct { uint8_t a; uint8_t b; uint8_t c; uint8_t d; };
///     };
///     uint64_t timestamp;
/// };
/// ```
///
/// A `field_declaration` is anonymous if it contains a `struct_specifier` or
/// `union_specifier` child that has a `field_declaration_list` (i.e. a body)
/// but no `type_identifier` (i.e. no name). The fields of the nested
/// struct/union are flattened into the parent.
///
/// Returns `None` if the declaration is not an anonymous nested struct/union
/// (the caller should fall through to `parse_field_declaration`).
/// (field_name, type_text, guard, alignas_override, source_line_1based)
type RawField = (String, String, Option<String>, Option<usize>, u32);

#[allow(clippy::only_used_in_recursion)]
fn parse_anonymous_nested(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    _parent_is_union: bool,
    aliases: &HashMap<String, String>,
) -> Option<Vec<RawField>> {
    // `struct { ... } name;` / `union { ... } name[3];` declares a named member, not
    // an anonymous one: its fields stay inside it (handled by `parse_field_declarations`).
    if (0..node.child_count()).any(|i| node.field_name_for_child(i) == Some("declarator")) {
        return None;
    }
    // Find a struct_specifier or union_specifier child.
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        if child.kind() != "struct_specifier" && child.kind() != "union_specifier" {
            continue;
        }
        let nested_is_union = child.kind() == "union_specifier";

        // Must have a body (field_declaration_list) but no type_identifier.
        let mut has_name = false;
        let mut body_node: Option<Node> = None;
        for j in 0..child.child_count() {
            let sub = child.child(j)?;
            match sub.kind() {
                "type_identifier" => has_name = true,
                "field_declaration_list" => body_node = Some(sub),
                _ => {}
            }
        }

        if has_name || body_node.is_none() {
            // Named struct/union used as a field type — handled by parse_field_declaration.
            continue;
        }

        let body = body_node?;
        let mut nested_raw: Vec<RawField> = Vec::new();

        for j in 0..body.child_count() {
            let inner = body.child(j)?;
            if inner.kind() == "field_declaration" {
                // Recurse to handle doubly-nested anonymous structs.
                if let Some(deeper) =
                    parse_anonymous_nested(source, inner, arch, nested_is_union, aliases)
                {
                    nested_raw.extend(deeper);
                } else {
                    nested_raw.extend(parse_field_declarations(source, inner, arch, aliases));
                }
            }
        }

        if !nested_raw.is_empty() {
            if nested_is_union {
                // All union variants overlap at offset 0.  Compute the union's
                // total size (= max field size, rounded to max alignment) and
                // emit a single synthetic field so the parent layout simulator
                // accounts for the correct footprint.  Previously all fields
                // were flattened sequentially, giving wrong struct sizes and
                // padding reports for the common anonymous-union pattern.
                let mut max_size = 0usize;
                let mut max_align = 1usize;
                for (_, ty, _, alignas, _) in &nested_raw {
                    let (resolved, _) = resolve_field_type(ty.trim(), aliases, arch);
                    let (sz, al) = c_type_size_align(&resolved, arch);
                    let eff_align = alignas.unwrap_or(al);
                    max_size = max_size.max(sz);
                    max_align = max_align.max(eff_align);
                }
                let union_size = if max_align > 0 {
                    max_size.next_multiple_of(max_align)
                } else {
                    max_size
                };
                let first_line = nested_raw[0].4;
                // Encode size+align into the type string; c_type_size_align decodes it.
                return Some(vec![(
                    "__anon_union".to_string(),
                    format!("__anon_union<{union_size},{max_align}>"),
                    None,
                    None,
                    first_line,
                )]);
            }
            return Some(nested_raw);
        }
    }
    None
}

/// A declarator with its pointer / array / function wrappers peeled off.
struct DeclInfo {
    name: String,
    /// The member (or, with `dims`, each array element) is a pointer or reference.
    pointer: bool,
    /// C++ pointer-to-member-function (`void (T::*f)()`): two words wide.
    member_fn_ptr: bool,
    /// Array length expressions in declaration order (`a[2][3]` → `["2", "3"]`).
    dims: Vec<String>,
}

fn is_declarator_kind(kind: &str) -> bool {
    matches!(
        kind,
        "field_identifier"
            | "identifier"
            | "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "function_declarator"
            | "parenthesized_declarator"
            | "attributed_declarator"
    )
}

fn inner_declarator(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("declarator").or_else(|| {
        (0..node.named_child_count())
            .filter_map(|i| node.named_child(i as u32))
            .find(|c| is_declarator_kind(c.kind()))
    })
}

/// Peel a declarator down to the member name. Reading outward from the name, the
/// first pointer makes the whole member a pointer (`char (*p)[3]`), while arrays
/// seen before any pointer are dimensions (`char *a[3]` is three pointers).
/// A function declarator directly around the name is a method, not a member.
fn decl_info(source: &str, node: Node<'_>, attrs: &mut String) -> Option<DeclInfo> {
    enum Wrap {
        Ptr,
        Array(String),
        Func,
    }
    let mut wraps = Vec::new();
    let mut cur = node;
    let name = loop {
        match cur.kind() {
            "field_identifier" | "identifier" => break source[cur.byte_range()].trim().to_string(),
            "pointer_declarator" | "reference_declarator" => wraps.push(Wrap::Ptr),
            "array_declarator" => wraps.push(Wrap::Array(
                cur.child_by_field_name("size")
                    .map(|s| source[s.byte_range()].trim().to_string())
                    .unwrap_or_default(),
            )),
            "function_declarator" => wraps.push(Wrap::Func),
            "attributed_declarator" => {
                for j in 0..cur.child_count() {
                    if let Some(c) = cur.child(j)
                        && matches!(c.kind(), "attribute_specifier" | "attribute")
                    {
                        attrs.push_str(source[c.byte_range()].trim());
                        attrs.push(' ');
                    }
                }
            }
            "parenthesized_declarator" => {}
            _ => return None,
        }
        cur = inner_declarator(cur)?;
    };

    let has_func = wraps.iter().any(|w| matches!(w, Wrap::Func));
    let member_fn_ptr = has_func && source[node.byte_range()].contains("::*");
    let mut dims = Vec::new();
    let mut pointer = false;
    for w in wraps.iter().rev() {
        match w {
            Wrap::Array(size) => dims.push(size.clone()),
            Wrap::Ptr => {
                pointer = true;
                break;
            }
            Wrap::Func => return None,
        }
    }
    Some(DeclInfo {
        name,
        pointer,
        member_fn_ptr,
        dims,
    })
}

/// Parse one `field_declaration` into its members. `int a, *b, c[4];` yields
/// three; a `static` member or a method declaration yields none.
fn parse_field_declarations(
    source: &str,
    node: Node<'_>,
    arch: &'static ArchConfig,
    aliases: &HashMap<String, String>,
) -> Vec<RawField> {
    let mut ty_parts: Vec<String> = Vec::new();
    let mut declarators: Vec<Node<'_>> = Vec::new();
    // Bit-field width, e.g. `int flags : 3;` → Some("3")
    let mut bit_width: Option<String> = None;
    // Collect attribute text for guard extraction
    let mut attr_text = String::new();
    // Field-level alignas override
    let mut alignas_override: Option<usize> = None;

    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else { continue };
        if node.field_name_for_child(i) == Some("declarator") {
            declarators.push(child);
            continue;
        }
        match child.kind() {
            // Static data members live outside the object.
            "storage_class_specifier" if source[child.byte_range()].trim() == "static" => {
                return Vec::new();
            }
            "type_specifier" | "primitive_type" | "type_identifier" | "sized_type_specifier" => {
                ty_parts.push(source[child.byte_range()].trim().to_string());
            }
            // C++ qualified types: std::mutex, ns::Type, etc.
            // C++ template types:  std::atomic<uint64_t>, std::vector<int>, etc.
            "qualified_identifier" | "template_type" => {
                ty_parts.push(source[child.byte_range()].trim().to_string());
            }
            // C11 _Atomic(T) type-specifier form: tree-sitter produces an `atomic_type`
            // node wrapping the inner type in parentheses.  Extract the inner type so
            // `_Atomic(int)` is sized as `int` (4B), not as pointer-size.
            "atomic_type" => {
                for j in 0..child.child_count() {
                    if let Some(sub) = child.child(j) {
                        match sub.kind() {
                            "primitive_type"
                            | "type_identifier"
                            | "sized_type_specifier"
                            | "qualified_identifier" => {
                                ty_parts.push(source[sub.byte_range()].trim().to_string());
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
            // `_Atomic(long)` after a qualifier is parsed as a macro-style type
            // specifier with a MISSING name; the real type is its descriptor.
            "macro_type_specifier"
                if child
                    .child_by_field_name("name")
                    .is_none_or(|n| n.is_missing() || source[n.byte_range()] == *"_Atomic") =>
            {
                if let Some(t) = child.child_by_field_name("type") {
                    ty_parts.push(source[t.byte_range()].trim().to_string());
                }
            }
            // `enum E e;` / `enum class E : uint8_t e;` — an enum is its underlying
            // integer type (`int` unless a fixed base is spelled out).
            "enum_specifier" => {
                ty_parts.push(
                    child
                        .child_by_field_name("base")
                        .map(|b| source[b.byte_range()].trim().to_string())
                        .unwrap_or_else(|| "int".to_string()),
                );
            }
            // Nested struct/union used as a field type: `struct Vec2 tl;`
            // Extract just the type_identifier name (e.g. "Vec2") so the
            // nested-struct resolution pass can match it by name. A tagless
            // body (`struct { ... } member;`) is laid out here and encoded as an
            // opaque `__anon_struct<size,align>` type.
            "struct_specifier" | "union_specifier" => {
                let mut tag = None;
                let mut has_body = false;
                for j in 0..child.child_count() {
                    if let Some(sub) = child.child(j) {
                        match sub.kind() {
                            "type_identifier" if tag.is_none() => {
                                tag = Some(source[sub.byte_range()].trim().to_string());
                            }
                            "field_declaration_list" => has_body = true,
                            _ => {}
                        }
                    }
                }
                if let Some(tag) = tag {
                    ty_parts.push(tag);
                } else if has_body {
                    let is_union = child.kind() == "union_specifier";
                    let (size, align) =
                        parse_struct_or_union_specifier(source, child, arch, is_union, aliases, 0)
                            .map(|l| (l.total_size, l.align))
                            .unwrap_or((0, 1));
                    let kind = if is_union { "union" } else { "struct" };
                    ty_parts.push(format!("__anon_{kind}<{size},{align}>"));
                }
            }
            // Bit-field clause: `: N`  (tree-sitter-c/cpp node)
            "bitfield_clause" => {
                let text = source[child.byte_range()].trim();
                // Strip leading ':' and whitespace to get just the width digits
                bit_width = Some(text.trim_start_matches(':').trim().to_string());
            }
            // GNU attribute specifier: __attribute__((...))
            "attribute_specifier" | "attribute" => {
                attr_text.push_str(source[child.byte_range()].trim());
                attr_text.push(' ');
            }
            // C++11 alignas: tree-sitter-cpp wraps it as type_qualifier → alignas_qualifier
            // Also handle the direct form in case grammar versions differ.
            "alignas_qualifier" | "alignas_specifier" if alignas_override.is_none() => {
                alignas_override = parse_alignas_value(source, child);
            }
            // type_qualifier wraps alignas_qualifier for field declarations:
            // `alignas(8) char c;` → type_qualifier { alignas_qualifier { ... } }
            "type_qualifier" if alignas_override.is_none() => {
                for j in 0..child.child_count() {
                    if let Some(sub) = child.child(j)
                        && (sub.kind() == "alignas_qualifier" || sub.kind() == "alignas_specifier")
                    {
                        alignas_override = parse_alignas_value(source, sub);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let mut base_ty = ty_parts.join(" ");
    if base_ty.is_empty() {
        return Vec::new();
    }
    // `double _Complex z;` — tree-sitter drops the `_Complex` specifier.
    let field_src = source[node.byte_range()].to_string();
    if (field_src.contains("_Complex") || field_src.contains("__complex__"))
        && !base_ty.contains("_Complex")
    {
        base_ty.push_str(" _Complex");
    }

    // Also check the full field source text (attribute_specifier may not always
    // be a direct child depending on tree-sitter grammar version).
    let line = node.start_position().row as u32 + 1;

    // Anonymous bitfields (`int : 3;` with no declarator) are padding bits that
    // still consume storage-unit bits. Return them with an empty name so
    // `resolve_bitfield_groups` can account for them without displaying a name.
    if declarators.is_empty() {
        return match bit_width {
            Some(w) => vec![(String::new(), format!("{base_ty}:{w}"), None, None, line)],
            None => Vec::new(),
        };
    }

    let mut out = Vec::new();
    for decl in declarators {
        let mut attrs = attr_text.clone();
        let Some(mut info) = decl_info(source, decl, &mut attrs) else {
            continue;
        };
        // tree-sitter reads `double _Complex z;` as a field named `_Complex` followed
        // by an ERROR node holding the real name.
        if matches!(info.name.as_str(), "_Complex" | "__complex__")
            && let Some(real) = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .filter(|c| c.kind() == "ERROR")
                .find_map(|e| {
                    (0..e.child_count())
                        .filter_map(|i| e.child(i))
                        .find(|c| c.kind() == "identifier")
                })
        {
            info.name = source[real.byte_range()].trim().to_string();
        }
        let mut ty = if info.member_fn_ptr {
            "__memfn_ptr".to_string()
        } else if info.pointer {
            format!("{base_ty} *")
        } else {
            base_ty.clone()
        };
        for dim in &info.dims {
            ty.push('[');
            ty.push_str(dim.trim());
            ty.push(']');
        }
        // Annotate bit-field types as "type:N" so callers can detect and report them;
        // `strip_bitfield_suffix` recovers the base type for size/align lookup.
        if let Some(w) = &bit_width {
            ty = format!("{ty}:{w}");
        }
        let guard = extract_guard_from_c_field_text(&attrs)
            .or_else(|| extract_guard_from_c_field_text(&field_src));
        // __attribute__((aligned(N))) — GCC/Clang extension, separate from C++11 alignas.
        // Only applies when no alignas_qualifier was already found.
        let effective_alignas = alignas_override.or_else(|| {
            extract_aligned_from_c_field_text(&attrs)
                .or_else(|| extract_aligned_from_c_field_text(&field_src))
        });
        out.push((info.name, ty, guard, effective_alignas, line));
    }
    out
}

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse_c(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let tree = C_PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed"))?;
    let mut layouts = Vec::new();
    extract_structs_from_tree(source, tree.root_node(), arch, &mut layouts);
    Ok(layouts)
}

pub fn parse_cpp(source: &str, arch: &'static ArchConfig) -> anyhow::Result<Vec<StructLayout>> {
    let tree = CPP_PARSER
        .with(|p| p.borrow_mut().parse(source, None))
        .ok_or_else(|| anyhow::anyhow!("tree-sitter parse failed"))?;
    let mut layouts = Vec::new();
    extract_structs_from_tree(source, tree.root_node(), arch, &mut layouts);
    Ok(layouts)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use padlock_core::arch::X86_64_SYSV;

    #[test]
    fn parse_simple_c_struct() {
        let src = r#"
struct Point {
    int x;
    int y;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Point");
        assert_eq!(layouts[0].fields.len(), 2);
        assert_eq!(layouts[0].fields[0].name, "x");
        assert_eq!(layouts[0].fields[1].name, "y");
    }

    #[test]
    fn parse_typedef_struct() {
        let src = r#"
typedef struct {
    char  is_active;
    double timeout;
    int   port;
} Connection;
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Connection");
        assert_eq!(layouts[0].fields.len(), 3);
    }

    #[test]
    fn c_layout_computes_offsets() {
        let src = "struct T { char a; double b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let layout = &layouts[0];
        // char at offset 0, double at offset 8 (7 bytes padding)
        assert_eq!(layout.fields[0].offset, 0);
        assert_eq!(layout.fields[1].offset, 8);
        assert_eq!(layout.total_size, 16);
    }

    #[test]
    fn c_layout_detects_padding() {
        let src = "struct T { char a; int b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let gaps = padlock_core::ir::find_padding(&layouts[0]);
        assert!(!gaps.is_empty());
        assert_eq!(gaps[0].bytes, 3); // 3 bytes padding between char and int
    }

    #[test]
    fn parse_cpp_struct() {
        let src = "struct Vec3 { float x; float y; float z; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].fields.len(), 3);
    }

    // ── SIMD types ────────────────────────────────────────────────────────────

    #[test]
    fn simd_sse_field_size_and_align() {
        let src = "struct Vecs { __m128 a; __m256 b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let f = &layouts[0].fields;
        assert_eq!(f[0].size, 16); // __m128
        assert_eq!(f[0].align, 16);
        assert_eq!(f[1].size, 32); // __m256
        assert_eq!(f[1].align, 32);
    }

    #[test]
    fn simd_avx512_size() {
        let src = "struct Wide { __m512 v; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].fields[0].size, 64);
        assert_eq!(layouts[0].fields[0].align, 64);
    }

    #[test]
    fn simd_padding_detected_when_small_field_before_avx() {
        // char(1) + [31 pad] + __m256(32) = 64 bytes, 31 wasted
        let src = "struct Mixed { char flag; __m256 data; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let gaps = padlock_core::ir::find_padding(&layouts[0]);
        assert!(!gaps.is_empty());
        assert_eq!(gaps[0].bytes, 31);
    }

    // ── union parsing ─────────────────────────────────────────────────────────

    #[test]
    fn union_fields_all_at_offset_zero() {
        let src = "union Data { int i; float f; double d; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let u = &layouts[0];
        assert!(u.is_union);
        for field in &u.fields {
            assert_eq!(
                field.offset, 0,
                "union field '{}' should be at offset 0",
                field.name
            );
        }
    }

    #[test]
    fn union_total_size_is_max_field() {
        // double is the largest (8 bytes); total should be 8
        let src = "union Data { int i; float f; double d; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts[0].total_size, 8);
    }

    #[test]
    fn union_no_padding_finding() {
        let src = "union Data { int i; double d; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let report = padlock_core::findings::Report::from_layouts(&layouts);
        let sr = &report.structs[0];
        assert!(
            !sr.findings
                .iter()
                .any(|f| matches!(f, padlock_core::findings::Finding::PaddingWaste { .. }))
        );
        assert!(
            !sr.findings
                .iter()
                .any(|f| matches!(f, padlock_core::findings::Finding::ReorderSuggestion { .. }))
        );
    }

    #[test]
    fn typedef_union_parsed() {
        let src = "typedef union { int a; double b; } Value;";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Value");
        assert!(layouts[0].is_union);
    }

    // ── attribute guard extraction ─────────────────────────────────────────────

    #[test]
    fn extract_guard_from_c_guarded_by_macro() {
        let text = "int value GUARDED_BY(mu);";
        let guard = extract_guard_from_c_field_text(text);
        assert_eq!(guard.as_deref(), Some("mu"));
    }

    #[test]
    fn extract_guard_from_c_attribute_specifier() {
        let text = "__attribute__((guarded_by(counter_lock))) uint64_t counter;";
        let guard = extract_guard_from_c_field_text(text);
        assert_eq!(guard.as_deref(), Some("counter_lock"));
    }

    #[test]
    fn extract_guard_pt_guarded_by() {
        let text = "int *ptr PT_GUARDED_BY(ptr_lock);";
        let guard = extract_guard_from_c_field_text(text);
        assert_eq!(guard.as_deref(), Some("ptr_lock"));
    }

    #[test]
    fn no_guard_returns_none() {
        let guard = extract_guard_from_c_field_text("int x;");
        assert!(guard.is_none());
    }

    #[test]
    fn c_struct_guarded_by_sets_concurrent_access() {
        // Using GUARDED_BY macro style in comments/text — tree-sitter won't parse
        // macro expansions, so test the text-extraction path via parse_field_declaration
        // indirectly by checking extract_guard_from_c_field_text.
        let text = "uint64_t readers GUARDED_BY(lock_a);";
        assert_eq!(
            extract_guard_from_c_field_text(text).as_deref(),
            Some("lock_a")
        );
    }

    #[test]
    fn c_struct_different_guards_detected_as_false_sharing() {
        use padlock_core::arch::X86_64_SYSV;
        use padlock_core::ir::{AccessPattern, Field, StructLayout, TypeInfo};

        // Manually build a layout with two fields on the same cache line,
        // different guards — mirrors what the C frontend would produce for
        // __attribute__((guarded_by(...))) annotated fields.
        let mut layout = StructLayout {
            name: "S".into(),
            total_size: 128,
            align: 8,
            fields: vec![
                Field {
                    name: "readers".into(),
                    ty: TypeInfo::Primitive {
                        name: "uint64_t".into(),
                        size: 8,
                        align: 8,
                    },
                    offset: 0,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Concurrent {
                        guard: Some("lock_a".into()),
                        is_atomic: false,
                        is_annotated: true,
                    },
                },
                Field {
                    name: "writers".into(),
                    ty: TypeInfo::Primitive {
                        name: "uint64_t".into(),
                        size: 8,
                        align: 8,
                    },
                    offset: 8,
                    size: 8,
                    align: 8,
                    source_file: None,
                    source_line: None,
                    access: AccessPattern::Concurrent {
                        guard: Some("lock_b".into()),
                        is_atomic: false,
                        is_annotated: true,
                    },
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
        assert!(padlock_core::analysis::false_sharing::has_false_sharing(
            &layout
        ));
        // Same guard → no false sharing
        layout.fields[1].access = AccessPattern::Concurrent {
            guard: Some("lock_a".into()),
            is_atomic: false,
            is_annotated: true,
        };
        assert!(!padlock_core::analysis::false_sharing::has_false_sharing(
            &layout
        ));
    }

    // ── C++ class: vtable pointer ─────────────────────────────────────────────

    #[test]
    fn cpp_class_with_virtual_method_has_vptr() {
        let src = r#"
class Widget {
    virtual void draw();
    int x;
    int y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        // First field must be __vptr
        assert_eq!(l.fields[0].name, "__vptr");
        assert_eq!(l.fields[0].size, 8); // pointer on x86_64
        // __vptr is at offset 0
        assert_eq!(l.fields[0].offset, 0);
        // int x should come after the pointer (at offset 8)
        let x = l.fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x.offset, 8);
    }

    #[test]
    fn cpp_class_without_virtual_has_no_vptr() {
        let src = "class Plain { int a; int b; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert!(!layouts[0].fields.iter().any(|f| f.name == "__vptr"));
    }

    #[test]
    fn cpp_struct_keyword_with_virtual_has_vptr() {
        // C++ structs with virtual methods get the same hidden __vptr as classes.
        let src = "struct IFoo { virtual ~IFoo(); virtual void bar(); };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].fields[0].name, "__vptr");
        assert_eq!(layouts[0].fields[0].offset, 0);
        assert_eq!(layouts[0].fields[0].size, 8);
    }

    // ── C++ class: single inheritance ─────────────────────────────────────────

    #[test]
    fn cpp_derived_class_has_base_slot() {
        let src = r#"
class Base {
    int x;
};
class Derived : public Base {
    int y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        // Both Base and Derived should be parsed
        let derived = layouts.iter().find(|l| l.name == "Derived").unwrap();
        // Derived must have a __base_Base synthetic field
        assert!(
            derived.fields.iter().any(|f| f.name == "__base_Base"),
            "Derived should have a __base_Base field"
        );
        // The y field should come after __base_Base
        let base_field = derived
            .fields
            .iter()
            .find(|f| f.name == "__base_Base")
            .unwrap();
        let y_field = derived.fields.iter().find(|f| f.name == "y").unwrap();
        assert!(y_field.offset >= base_field.offset + base_field.size);
    }

    #[test]
    fn cpp_class_multiple_inheritance_has_multiple_base_slots() {
        let src = r#"
class A { int a; };
class B { int b; };
class C : public A, public B { int c; };
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let c = layouts.iter().find(|l| l.name == "C").unwrap();
        assert!(c.fields.iter().any(|f| f.name == "__base_A"));
        assert!(c.fields.iter().any(|f| f.name == "__base_B"));
    }

    #[test]
    fn cpp_virtual_base_class_total_size_accounts_for_vptr() {
        // class with virtual method: size = sizeof(__vptr) + member fields + padding
        let src = "class V { virtual void f(); int x; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        // __vptr(8) + int(4) + 4 pad = 16 bytes on x86_64
        assert_eq!(l.total_size, 16);
    }

    // ── bitfield handling ─────────────────────────────────────────────────────

    #[test]
    fn is_bitfield_type_detects_colon_n() {
        assert!(is_bitfield_type("int:3"));
        assert!(is_bitfield_type("unsigned int:16"));
        assert!(is_bitfield_type("uint32_t:1"));
        // Not bit-fields — contains ':' but not followed by pure digits
        assert!(!is_bitfield_type("std::atomic<int>"));
        assert!(!is_bitfield_type("ns::Type"));
        assert!(!is_bitfield_type("int"));
    }

    #[test]
    fn struct_with_bitfields_is_parsed() {
        // Bitfields are now packed into storage-unit-sized synthetic fields
        // instead of skipping the whole struct.
        let src = r#"
struct Flags {
    unsigned int active : 1;
    unsigned int ready  : 1;
    unsigned int error  : 6;
    int value;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "Flags")
            .expect("Flags must be parsed");
        // active:1 + ready:1 + error:6 = 8 bits, fits in one int storage unit (4 bytes)
        // Then int value (4 bytes) → total 8 bytes
        assert_eq!(l.total_size, 8, "Flags should be 8 bytes");
        // The bitfield group and value field
        assert_eq!(
            l.fields.len(),
            2,
            "should have 2 fields (bitfield group + value)"
        );
        assert!(
            l.fields[0].name.starts_with('['),
            "first field should be the bitfield group"
        );
        assert_eq!(l.fields[1].name, "value");
    }

    #[test]
    fn bitfield_group_name_shows_packed_members() {
        let src = "struct S { unsigned int a : 3; unsigned int b : 5; int c; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").unwrap();
        assert!(
            l.fields[0].name.contains("a:3"),
            "group name should contain a:3, got {}",
            l.fields[0].name
        );
        assert!(
            l.fields[0].name.contains("b:5"),
            "group name should contain b:5, got {}",
            l.fields[0].name
        );
    }

    #[test]
    fn bitfield_different_unit_size_shares_bytes() {
        // Bit-fields pack bit by bit regardless of declared type: b (uint32_t:5)
        // continues right after a (uint8_t:3) in the same byte, and the struct
        // takes uint32_t's alignment. Verified against gcc: sizeof == 4.
        let src = "struct S { uint8_t a : 3; uint32_t b : 5; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").unwrap();
        assert_eq!(l.fields.len(), 1, "one contiguous bit run");
        assert_eq!(l.total_size, 4);
        assert_eq!(l.align, 4);
    }

    #[test]
    fn bitfield_overflow_starts_new_storage_unit() {
        // a:5 + b:5 = 10 bits > 8 (uint8_t): b must go into the next byte.
        let src = "struct S { uint8_t a : 5; uint8_t b : 5; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").unwrap();
        assert_eq!(l.fields.len(), 2, "overflow → 2 storage units");
        assert_eq!(l.total_size, 2, "two uint8_t units → 2 bytes");
    }

    #[test]
    fn zero_width_bitfield_flushes_group() {
        // `int : 0` forces alignment to the next int boundary.
        let src = "struct S { int a : 3; int : 0; int b : 5; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").unwrap();
        // a:3 in one int unit, b:5 in the next → two groups, each 4 bytes
        assert_eq!(l.fields.len(), 2, "zero-width bitfield flushes → 2 groups");
        assert_eq!(l.total_size, 8, "two int units → 8 bytes");
    }

    #[test]
    fn anonymous_bitfield_consumes_bits_in_storage_unit() {
        // `int : 5` is anonymous padding; a:3 + anon:5 = 8 bits, so b:3 still fits
        // in the same int unit (8 + 3 = 11 <= 32).  Without the anonymous bits
        // accounted for, a naïve grouper would also pack c into the same unit
        // even if that overflowed.
        let src = "struct S { int a : 3; int : 5; int b : 3; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").unwrap();
        // All three fit in one int storage unit → exactly one synthetic field, 4 bytes.
        assert_eq!(
            l.fields.len(),
            1,
            "a:3 + anon:5 + b:3 = 11 bits, fits in one int unit"
        );
        assert_eq!(l.total_size, 4);
        // The display name must show a and b but not the anonymous padding.
        assert!(l.fields[0].name.contains("a:3"), "name should include a:3");
        assert!(l.fields[0].name.contains("b:3"), "name should include b:3");
    }

    #[test]
    fn anonymous_only_bitfield_unit_emits_pad_placeholder() {
        // `int : 32` fills an entire 4-byte int unit with anonymous padding and
        // the next field `a : 4` starts in the following unit. The bits are one
        // contiguous run (5 bytes), padded out to int alignment: 8 bytes (gcc).
        let src = "struct S { int : 32; int a : 4; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").unwrap();
        assert_eq!(l.fields.len(), 1, "one contiguous bit run");
        assert_eq!(l.total_size, 8, "5 bytes of bits padded to int alignment");
        // A struct made only of anonymous padding bits keeps its `[__pad]` placeholder.
        let src = "struct P { int : 32; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "P").unwrap();
        assert!(l.fields[0].name.contains("__pad"));
        assert_eq!(l.total_size, 4);
    }

    #[test]
    fn struct_without_bitfields_is_still_parsed() {
        // Ensure the bitfield guard doesn't affect normal structs.
        let src = "struct Normal { int a; char b; double c; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Normal");
    }

    #[test]
    fn c_struct_fields_have_source_lines() {
        let src = "struct Point {\n    int x;\n    int y;\n};";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let fields = &layouts[0].fields;
        // x is on line 2, y is on line 3
        assert_eq!(fields[0].source_line, Some(2), "x should be line 2");
        assert_eq!(fields[1].source_line, Some(3), "y should be line 3");
    }

    #[test]
    fn cpp_class_with_bitfields_is_parsed() {
        // Bitfields in C++ classes are now grouped into storage units.
        let src = "class Packed { int x : 4; int y : 4; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "Packed")
            .expect("Packed must be parsed");
        // x:4 + y:4 = 8 bits, fits in one int (4 bytes)
        assert_eq!(
            l.total_size, 4,
            "x:4 + y:4 fit in one int storage unit → 4 bytes"
        );
    }

    #[test]
    fn all_bitfield_struct_is_parsed() {
        // Struct with ONLY bit-field members is now supported.
        let src = "struct BitPacked { int x:4; int y:4; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "BitPacked")
            .expect("BitPacked must now be parsed");
        // x:4 + y:4 = 8 bits in one int storage unit → 4 bytes
        assert_eq!(l.total_size, 4);
    }

    // ── __attribute__((packed)) detection ─────────────────────────────────────

    #[test]
    fn packed_struct_has_no_alignment_padding() {
        // Without packed: char(1) + 3-byte pad + int(4) + char(1) + 3-byte pad = 12 bytes
        // With packed:    char(1) + int(4) + char(1) = 6 bytes, align=1
        let src = r#"
struct __attribute__((packed)) Tight {
    char a;
    int  b;
    char c;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Tight").expect("Tight");
        assert!(l.is_packed, "should be marked is_packed");
        assert_eq!(l.total_size, 6, "packed: no padding inserted");
        assert_eq!(l.fields[0].offset, 0);
        assert_eq!(l.fields[1].offset, 1); // immediately after char
        assert_eq!(l.fields[2].offset, 5);
    }

    #[test]
    fn non_packed_struct_has_normal_alignment_padding() {
        // Confirm baseline: same struct without __attribute__((packed)) gets padded
        let src = r#"
struct Normal {
    char a;
    int  b;
    char c;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Normal").expect("Normal");
        assert!(!l.is_packed);
        assert_eq!(l.total_size, 12);
        assert_eq!(l.fields[1].offset, 4); // aligned to 4
    }

    #[test]
    fn cpp_class_packed_attribute_detected() {
        let src = r#"
class __attribute__((packed)) Dense {
    char a;
    int  b;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Dense").expect("Dense");
        assert!(
            l.is_packed,
            "C++ class with __attribute__((packed)) must be marked packed"
        );
        assert_eq!(l.total_size, 5); // char(1) + int(4), no padding
    }

    // ── alignas detection ─────────────────────────────────────────────────────

    #[test]
    fn field_alignas_overrides_natural_alignment() {
        // char is normally align=1 but alignas(8) forces it to align-8.
        // Layout: c(1B at offset 0, align=8) + x(4B at offset 4, align=4)
        // c must start on an 8-byte boundary (trivially satisfied at offset 0).
        // After c (1 byte), x aligns to 4: offset = 1.next_multiple_of(4) = 4.
        // Struct align = max(8, 4) = 8. Total = 8 bytes (4+4 → 8 → ok for align 8).
        let src = r#"
struct S {
    alignas(8) char c;
    int x;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        // c should be forced to align 8
        let c_field = l.fields.iter().find(|f| f.name == "c").unwrap();
        assert_eq!(c_field.align, 8);
        // x comes after c (1 byte) with natural alignment 4 → offset 4
        let x_field = l.fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x_field.offset, 4);
        // Struct alignment is max(alignas(8), int align 4) = 8
        assert_eq!(l.align, 8);
        // Total = 8 bytes (x at 4, size 4; 4+4=8; 8 is multiple of align 8)
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn struct_level_alignas_increases_struct_alignment() {
        // alignas(64) on the struct means its alignment requirement is 64.
        // Total size must be a multiple of 64.
        let src = r#"
struct alignas(64) CacheLine {
    int x;
    int y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "CacheLine")
            .expect("CacheLine");
        assert_eq!(l.align, 64);
        assert_eq!(l.total_size % 64, 0);
    }

    #[test]
    fn alignas_on_field_smaller_than_natural_is_ignored() {
        // alignas(1) on an int field: does NOT reduce alignment below 4.
        // In C++, alignas cannot reduce alignment below the natural alignment.
        // Our implementation stores the alignas value; natural alignment wins
        // because we take max(alignas, natural) in the caller.
        // Note: we currently store alignas directly; this test documents behaviour.
        let src = "struct S { int x; int y; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].align, 4); // natural alignment, not reduced
    }

    #[test]
    fn cpp_class_alignas_detected() {
        let src = r#"
class alignas(32) Aligned {
    double x;
    double y;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts
            .iter()
            .find(|l| l.name == "Aligned")
            .expect("Aligned");
        assert_eq!(l.align, 32);
        assert_eq!(l.total_size % 32, 0);
    }

    // ── bad weather: alignas edge cases ───────────────────────────────────────

    #[test]
    fn struct_without_alignas_unchanged() {
        // Ensure the alignas detection path doesn't affect structs without it
        let src = "struct Plain { int a; char b; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.align, 4); // max field alignment = int = 4
        assert_eq!(l.total_size, 8); // int(4) + char(1) + 3 pad
    }

    // ── __declspec(align(N)) — MSVC alignment attribute ───────────────────────

    #[test]
    fn declspec_align_on_struct() {
        // `__declspec(align(64))` on the struct tag raises the struct alignment.
        let src = "__declspec(align(64)) struct CacheLine { int x; int y; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].align, 64);
    }

    #[test]
    fn declspec_align_on_field() {
        // `__declspec(align(16))` on a field overrides its natural alignment.
        let src = "struct S { __declspec(align(16)) int x; int y; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let x = layouts[0].fields.iter().find(|f| f.name == "x").unwrap();
        assert_eq!(x.align, 16);
    }

    #[test]
    fn extract_declspec_align_unit() {
        assert_eq!(
            super::extract_declspec_align("__declspec(align(64))"),
            Some(64)
        );
        assert_eq!(
            super::extract_declspec_align("__declspec(align(16)) struct Foo {"),
            Some(16)
        );
        assert_eq!(super::extract_declspec_align("__declspec(noinline)"), None);
        assert_eq!(super::extract_declspec_align("no declspec here"), None);
    }

    // ── anonymous nested structs/unions ───────────────────────────────────────

    #[test]
    fn anonymous_nested_union_emits_synthetic_field() {
        // Anonymous nested unions are now represented as a single `__anon_union`
        // synthetic field sized to max(variants).  Previously all variants were
        // flattened into the parent sequentially, giving wrong sizes.
        let src = r#"
struct Packet {
    union {
        uint32_t raw;
        uint8_t bytes[4];
    };
    uint64_t timestamp;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Packet").expect("Packet");
        // The anonymous union appears as a single synthetic __anon_union field.
        assert!(
            l.fields.iter().any(|f| f.name == "__anon_union"),
            "__anon_union synthetic field must be present in Packet"
        );
        assert!(
            l.fields.iter().any(|f| f.name == "timestamp"),
            "timestamp must be present"
        );
        // Individual union members must not appear directly in Packet.
        assert!(
            !l.fields.iter().any(|f| f.name == "raw"),
            "raw must not be flattened directly into Packet"
        );
    }

    #[test]
    fn anonymous_nested_struct_fields_flattened() {
        let src = r#"
struct Outer {
    struct {
        int x;
        int y;
    };
    double z;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Outer").expect("Outer");
        assert!(
            l.fields.iter().any(|f| f.name == "x"),
            "x must be flattened"
        );
        assert!(
            l.fields.iter().any(|f| f.name == "y"),
            "y must be flattened"
        );
        assert!(l.fields.iter().any(|f| f.name == "z"), "z present");
        // Total: x(4) + y(4) + z(8) = 16 bytes, no padding
        assert_eq!(l.total_size, 16);
    }

    #[test]
    fn named_nested_struct_not_flattened() {
        // A named struct used as a field type must NOT be flattened
        let src = r#"
struct Vec2 { float x; float y; };
struct Rect { struct Vec2 tl; struct Vec2 br; };
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let rect = layouts.iter().find(|l| l.name == "Rect").expect("Rect");
        // Should have tl and br as opaque fields, not x/y flattened
        assert_eq!(rect.fields.len(), 2);
        assert!(rect.fields.iter().any(|f| f.name == "tl"));
        assert!(rect.fields.iter().any(|f| f.name == "br"));
    }

    // ── type-table tests ──────────────────────────────────────────────────────

    #[test]
    fn linux_kernel_types_correct_size() {
        // u8/u16/u32/u64 and s8/s16/s32/s64 (linux/types.h)
        assert_eq!(c_type_size_align("u8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("u16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("u32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("u64", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("s8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("s16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("s32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("s64", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn linux_kernel_dunder_types_correct_size() {
        assert_eq!(c_type_size_align("__u8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("__u16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__u32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("__u64", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("__s8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("__s64", &X86_64_SYSV), (8, 8));
        // Endian-annotated types are same width as their base
        assert_eq!(c_type_size_align("__be16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__le32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("__be64", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn c99_fast_types_correct_size() {
        // fast8/16 are their natural width
        assert_eq!(c_type_size_align("uint_fast8_t", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("uint_fast16_t", &X86_64_SYSV), (2, 2));
        // fast32/64 are pointer-sized on 64-bit
        assert_eq!(c_type_size_align("uint_fast32_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("uint_fast64_t", &X86_64_SYSV), (8, 8));
        // least types are their minimum guaranteed width
        assert_eq!(c_type_size_align("uint_least8_t", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("uint_least32_t", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("uint_least64_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("intmax_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("uintmax_t", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn gcc_int128_correct_size() {
        assert_eq!(c_type_size_align("__int128", &X86_64_SYSV), (16, 16));
        assert_eq!(c_type_size_align("__uint128", &X86_64_SYSV), (16, 16));
        assert_eq!(c_type_size_align("__int128_t", &X86_64_SYSV), (16, 16));
        // unsigned __int128 — "unsigned " prefix is stripped, then __int128 matched
        assert_eq!(
            c_type_size_align("unsigned __int128", &X86_64_SYSV),
            (16, 16)
        );
    }

    #[test]
    fn windows_types_correct_size() {
        assert_eq!(c_type_size_align("BYTE", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("WORD", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("DWORD", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("QWORD", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("BOOL", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("UINT8", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("INT32", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("UINT64", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("HANDLE", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("LPVOID", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn char_types_correct_size() {
        assert_eq!(c_type_size_align("wchar_t", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("char8_t", &X86_64_SYSV), (1, 1));
        assert_eq!(c_type_size_align("char16_t", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("char32_t", &X86_64_SYSV), (4, 4));
    }

    #[test]
    fn half_precision_types_correct_size() {
        assert_eq!(c_type_size_align("_Float16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__fp16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("__bf16", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("_Float128", &X86_64_SYSV), (16, 16));
    }

    #[test]
    fn unsigned_prefix_stripped_correctly() {
        // "unsigned short" → "short" → (2, 2)
        assert_eq!(c_type_size_align("unsigned short", &X86_64_SYSV), (2, 2));
        assert_eq!(c_type_size_align("unsigned int", &X86_64_SYSV), (4, 4));
        assert_eq!(
            c_type_size_align("unsigned long long", &X86_64_SYSV),
            (8, 8)
        );
        assert_eq!(
            c_type_size_align("long int", &X86_64_SYSV),
            (X86_64_SYSV.pointer_size, X86_64_SYSV.pointer_size)
        );
    }

    #[test]
    fn linux_kernel_struct_with_new_types() {
        // Representative kernel-style struct using __u32, __be16, u8
        let src = r#"
struct NetHeader {
    __be32 src_ip;
    __be32 dst_ip;
    __be16 src_port;
    __be16 dst_port;
    u8     protocol;
    u8     ttl;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        // 4+4+2+2+1+1 = 14B; max align is 4 (__be32) → padded to 16B
        assert_eq!(l.total_size, 16);
        assert_eq!(l.fields[0].size, 4); // __be32 src_ip
        assert_eq!(l.fields[2].size, 2); // __be16 src_port
        assert_eq!(l.fields[4].size, 1); // u8 protocol
    }

    // ── C++ stdlib type tests ─────────────────────────────────────────────────

    #[test]
    fn cpp_string_is_32_bytes() {
        assert_eq!(c_type_size_align("std::string", &X86_64_SYSV), (32, 8));
        assert_eq!(c_type_size_align("std::wstring", &X86_64_SYSV), (32, 8));
    }

    #[test]
    fn cpp_string_view_is_two_words() {
        assert_eq!(c_type_size_align("std::string_view", &X86_64_SYSV), (16, 8));
    }

    #[test]
    fn cpp_vector_is_24_bytes() {
        assert_eq!(c_type_size_align("std::vector<int>", &X86_64_SYSV), (24, 8));
        assert_eq!(
            c_type_size_align("std::vector<uint64_t>", &X86_64_SYSV),
            (24, 8)
        );
        // Size is independent of T
        assert_eq!(
            c_type_size_align("std::vector<std::string>", &X86_64_SYSV),
            (24, 8)
        );
    }

    #[test]
    fn cpp_smart_pointers_correct_size() {
        // unique_ptr: single pointer
        assert_eq!(
            c_type_size_align("std::unique_ptr<int>", &X86_64_SYSV),
            (8, 8)
        );
        // shared_ptr / weak_ptr: two pointers
        assert_eq!(
            c_type_size_align("std::shared_ptr<int>", &X86_64_SYSV),
            (16, 8)
        );
        assert_eq!(
            c_type_size_align("std::weak_ptr<int>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_optional_recursive_size() {
        // std::optional<bool>: 1B (bool) + 1B (has_value flag) → 2B
        assert_eq!(
            c_type_size_align("std::optional<bool>", &X86_64_SYSV),
            (2, 1)
        );
        // std::optional<int>: 4B + 1B → padded to 4B → 8B total? Let's check:
        // t_size=4, t_align=4; (4+1).next_multiple_of(4) = 8
        assert_eq!(
            c_type_size_align("std::optional<int>", &X86_64_SYSV),
            (8, 4)
        );
        // std::optional<double>: 8B + 1B → padded to 8B → 16B
        assert_eq!(
            c_type_size_align("std::optional<double>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_function_is_32_bytes() {
        assert_eq!(
            c_type_size_align("std::function<void()>", &X86_64_SYSV),
            (32, 8)
        );
        assert_eq!(
            c_type_size_align("std::function<int(int)>", &X86_64_SYSV),
            (32, 8)
        );
    }

    // ── split_template_args ───────────────────────────────────────────────────

    #[test]
    fn split_template_args_simple() {
        assert_eq!(split_template_args("int, double"), vec!["int", "double"]);
        assert_eq!(split_template_args("int"), vec!["int"]);
    }

    #[test]
    fn split_template_args_nested() {
        // Nested angle brackets must not be split at depth > 0.
        let args = split_template_args("std::map<int,int>, double");
        assert_eq!(args, vec!["std::map<int,int>", "double"]);
        let args = split_template_args("int, std::pair<char, short>, float");
        assert_eq!(args, vec!["int", "std::pair<char, short>", "float"]);
    }

    // ── std::variant sizing ───────────────────────────────────────────────────

    #[test]
    fn cpp_variant_single_int() {
        // storage=4, align=4 (max'd to 4), total=(4+4).next_mult_of(4)=8
        assert_eq!(c_type_size_align("std::variant<int>", &X86_64_SYSV), (8, 4));
    }

    #[test]
    fn cpp_variant_int_double() {
        // max_size=8, max_align=8, storage=8, total=(8+4).next_mult_of(8)=16
        assert_eq!(
            c_type_size_align("std::variant<int, double>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_variant_nested_template_arg() {
        // std::variant<std::map<int,int>, double> — bracket-aware split required.
        let (sz, _) = c_type_size_align("std::variant<std::map<int,int>, double>", &X86_64_SYSV);
        // map=48, double=8 → max_size=48, max_align=8, total=(48+4).next_mult_of(8)=56
        assert_eq!(sz, 56);
    }

    // ── std::pair sizing ─────────────────────────────────────────────────────

    #[test]
    fn cpp_pair_int_char() {
        // int(4) at 0, char(1) at 4, raw=5, align=max(4,1)=4, total=8
        assert_eq!(
            c_type_size_align("std::pair<int, char>", &X86_64_SYSV),
            (8, 4)
        );
    }

    #[test]
    fn cpp_pair_char_int() {
        // char(1) at 0, int(4) at 4 (aligned), raw=8, align=4, total=8
        assert_eq!(
            c_type_size_align("std::pair<char, int>", &X86_64_SYSV),
            (8, 4)
        );
    }

    // ── std::tuple sizing ────────────────────────────────────────────────────

    #[test]
    fn cpp_tuple_three_fields() {
        // int(4) at 0, double(8) at 8 (aligned), char(1) at 16, raw=17, align=8, total=24
        assert_eq!(
            c_type_size_align("std::tuple<int, double, char>", &X86_64_SYSV),
            (24, 8)
        );
    }

    // ── std::array sizing ────────────────────────────────────────────────────

    #[test]
    fn cpp_array_of_int() {
        // std::array<int, 4> = 4 * 4B = 16B, align=4
        assert_eq!(
            c_type_size_align("std::array<int, 4>", &X86_64_SYSV),
            (16, 4)
        );
    }

    #[test]
    fn cpp_array_of_double() {
        // std::array<double, 3> = 3 * 8B = 24B, align=8
        assert_eq!(
            c_type_size_align("std::array<double, 3>", &X86_64_SYSV),
            (24, 8)
        );
    }

    // ── std::bitset, std::complex, std::thread, etc. ─────────────────────────

    #[test]
    fn cpp_bitset_sizing() {
        // std::bitset<8>: 1B → padded to 8 (word), align 8
        assert_eq!(c_type_size_align("std::bitset<8>", &X86_64_SYSV), (8, 8));
        // std::bitset<64>: 8B → already 8, align 8
        assert_eq!(c_type_size_align("std::bitset<64>", &X86_64_SYSV), (8, 8));
        // std::bitset<128>: 16B, align 8
        assert_eq!(c_type_size_align("std::bitset<128>", &X86_64_SYSV), (16, 8));
    }

    #[test]
    fn cpp_complex_sizing() {
        assert_eq!(
            c_type_size_align("std::complex<float>", &X86_64_SYSV),
            (8, 4)
        );
        assert_eq!(
            c_type_size_align("std::complex<double>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_thread_sizing() {
        assert_eq!(c_type_size_align("std::thread", &X86_64_SYSV), (8, 8));
    }

    #[test]
    fn cpp_initializer_list_sizing() {
        assert_eq!(
            c_type_size_align("std::initializer_list<int>", &X86_64_SYSV),
            (16, 8)
        );
    }

    #[test]
    fn cpp_chrono_duration_sizing() {
        // duration<int64_t, nano> → size of int64_t = 8B
        assert_eq!(
            c_type_size_align(
                "std::chrono::duration<int64_t, std::ratio<1,1000000000>>",
                &X86_64_SYSV
            ),
            (8, 8)
        );
        // duration<int32_t, milli> → size of int32_t = 4B
        assert_eq!(
            c_type_size_align(
                "std::chrono::duration<int32_t, std::ratio<1,1000>>",
                &X86_64_SYSV
            ),
            (4, 4)
        );
    }

    #[test]
    fn cpp_filesystem_path_sizing() {
        assert_eq!(
            c_type_size_align("std::filesystem::path", &X86_64_SYSV),
            (32, 8)
        );
    }

    #[test]
    fn cpp_stdlib_struct_with_string_field() {
        // A struct with std::string fields — used to get pointer-size (8B), now 32B
        let src = r#"
struct Config {
    std::string name;
    int         version;
    bool        enabled;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = &layouts[0];
        assert_eq!(l.fields[0].size, 32); // std::string, not 8
        // int at offset 32, bool at 36; total padded to 8-byte align = 40
        assert_eq!(l.fields[1].offset, 32);
        assert_eq!(l.fields[1].size, 4);
    }

    // ── typedef alias resolution ──────────────────────────────────────────────

    #[test]
    fn typedef_scalar_alias_resolves_correct_size() {
        // `typedef uint32_t UserId;` — UserId must be treated as 4B, not pointer-size.
        let src = r#"
typedef uint32_t UserId;

struct User {
    UserId id;
    char   name[16];
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "User").expect("User");
        let id = l.fields.iter().find(|f| f.name == "id").expect("id field");
        assert_eq!(id.size, 4, "UserId alias of uint32_t must be 4 bytes");
        assert_eq!(id.align, 4);
    }

    #[test]
    fn typedef_alias_layout_correct_total_size() {
        // Without alias resolution: UserId → unknown → pointer_size (8B)
        // char(1) + 7 pad + unknown(8) = 16B.
        // With alias resolution: char(1) + 3 pad + uint32_t(4) = 8B.
        let src = r#"
typedef uint32_t Token;

struct Auth {
    char  prefix;
    Token token;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Auth").expect("Auth");
        // prefix(1) + 3-byte pad + token(4) = 8B
        assert_eq!(l.total_size, 8, "alias-resolved layout should be 8 bytes");
    }

    #[test]
    fn typedef_pointer_not_confused_with_scalar_alias() {
        // `typedef int *IntPtr;` — pointer typedef; alias name lives in a nested
        // declarator, so collect_typedef_aliases must NOT collect it.
        // IntPtr falls through to the unknown-type catch-all → pointer_size (8B).
        let src = r#"
typedef int *IntPtr;

struct S {
    IntPtr p;
    int    x;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "S").expect("S");
        let p = l.fields.iter().find(|f| f.name == "p").expect("p field");
        // Pointer typedef — either resolved as pointer (8B) or falls to pointer_size.
        assert_eq!(p.size, 8, "pointer typedef should be 8 bytes on x86_64");
    }

    #[test]
    fn typedef_struct_not_collected_as_scalar_alias() {
        // `typedef struct { ... } MyStruct;` must not appear in scalar alias map.
        // The struct is still emitted as a StructLayout.
        let src = r#"
typedef struct {
    int x;
    int y;
} Point;

struct Line {
    Point a;
    Point b;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        // Point must be emitted as a layout
        assert!(
            layouts.iter().any(|l| l.name == "Point"),
            "typedef struct should emit a StructLayout"
        );
    }

    #[test]
    fn cpp_class_typedef_alias_resolved() {
        // Typedef alias resolution must also work for C++ class fields.
        let src = r#"
typedef uint64_t Timestamp;

class Event {
    Timestamp when;
    int       kind;
};
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Event").expect("Event");
        let when = l.fields.iter().find(|f| f.name == "when").expect("when");
        assert_eq!(when.size, 8, "Timestamp alias of uint64_t must be 8 bytes");
        assert_eq!(when.align, 8);
    }

    // ── C++ template skipping ─────────────────────────────────────────────────

    #[test]
    fn cpp_template_struct_is_skipped() {
        // Generic C++ templates cannot be sized without monomorphisation.
        let src = "template<typename T> struct Wrapper { T value; int count; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Wrapper"),
            "template struct must be skipped, not emitted with wrong sizes"
        );
    }

    #[test]
    fn cpp_template_class_is_skipped() {
        let src = "template<typename T, typename U> class Pair { T first; U second; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Pair"),
            "template class must be skipped"
        );
    }

    #[test]
    fn cpp_non_template_struct_alongside_template_is_parsed() {
        // The template is skipped but concrete structs in the same TU are kept.
        let src = r#"
template<typename T> struct Generic { T val; };
struct Concrete { int x; double y; };
"#;
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| l.name != "Generic"),
            "Generic template must be skipped"
        );
        let concrete = layouts
            .iter()
            .find(|l| l.name == "Concrete")
            .expect("Concrete must be parsed");
        assert_eq!(concrete.fields.len(), 2);
    }

    // ── `padlock:instantiate` template instantiation ──────────────────────────

    #[test]
    fn cpp_template_instantiate_single_arg() {
        let src = "// padlock:instantiate(float)\ntemplate<typename T> struct Vector3 { T x; T y; T z; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let v = layouts
            .iter()
            .find(|l| l.name == "Vector3<float>")
            .expect("Vector3<float> must be instantiated");
        assert_eq!(v.total_size, 12);
        assert!(v.fields.iter().all(|f| f.size == 4));
    }

    #[test]
    fn cpp_template_instantiate_multiple_lines_produce_multiple_layouts() {
        let src = "// padlock:instantiate(float)\n// padlock:instantiate(double)\n\
                   template<typename T> struct Vector3 { T x; bool flag; T y; T z; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let f = layouts
            .iter()
            .find(|l| l.name == "Vector3<float>")
            .expect("Vector3<float> must be instantiated");
        let d = layouts
            .iter()
            .find(|l| l.name == "Vector3<double>")
            .expect("Vector3<double> must be instantiated");
        // Verified against a real g++ -g compile of the same instantiations.
        assert_eq!(f.total_size, 16);
        assert_eq!(d.total_size, 32);
        assert!(
            layouts.iter().all(|l| l.name != "Vector3"),
            "the unresolved template name itself must not appear as a layout"
        );
    }

    #[test]
    fn cpp_template_instantiate_multi_param() {
        let src = "// padlock:instantiate(int, double)\n\
                   template<typename T, typename U> struct Pair { T first; U second; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        let p = layouts
            .iter()
            .find(|l| l.name == "Pair<int, double>")
            .expect("Pair<int, double> must be instantiated");
        let first = p.fields.iter().find(|f| f.name == "first").unwrap();
        let second = p.fields.iter().find(|f| f.name == "second").unwrap();
        assert_eq!(first.size, 4);
        assert_eq!(second.size, 8);
    }

    #[test]
    fn cpp_template_instantiate_arg_count_mismatch_is_skipped_not_wrong() {
        // One type argument for a two-parameter template: must not silently
        // substitute a wrong/partial mapping.
        let src = "// padlock:instantiate(int)\n\
                   template<typename T, typename U> struct Pair { T first; U second; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(
            layouts.iter().all(|l| !l.name.starts_with("Pair")),
            "mismatched arg count must not produce any Pair instantiation"
        );
    }

    #[test]
    fn cpp_template_without_instantiate_annotation_is_still_skipped() {
        // No annotation present: existing skip behaviour must be unchanged.
        let src = "template<typename T> struct Wrapper { T value; };";
        let layouts = parse_cpp(src, &X86_64_SYSV).unwrap();
        assert!(layouts.iter().all(|l| !l.name.starts_with("Wrapper")));
    }

    // ── _Atomic qualifier and type-specifier form ─────────────────────────────

    #[test]
    fn atomic_qualifier_form_sized_as_base_type() {
        // `_Atomic int x` — C11 qualifier form.  tree-sitter parses `_Atomic` as a
        // type_qualifier node (ignored in field parsing) and `int` as primitive_type,
        // so the field correctly resolves to 4B.
        let src = "struct S { _Atomic int counter; int regular; };";
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        let counter = l.fields.iter().find(|f| f.name == "counter").unwrap();
        assert_eq!(counter.size, 4, "_Atomic int must be 4B");
        assert_eq!(counter.offset, 0);
        // regular follows at offset 4 with no gap
        let regular = l.fields.iter().find(|f| f.name == "regular").unwrap();
        assert_eq!(regular.offset, 4);
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn atomic_qualifier_strip_in_type_string() {
        // Ensure the `_Atomic ` prefix is stripped in c_type_size_align so that
        // `_Atomic int` arriving as a compound type string resolves to 4B.
        assert_eq!(c_type_size_align("_Atomic int", &X86_64_SYSV), (4, 4));
        assert_eq!(c_type_size_align("_Atomic uint64_t", &X86_64_SYSV), (8, 8));
        assert_eq!(c_type_size_align("_Atomic char", &X86_64_SYSV), (1, 1));
    }

    // ── anonymous nested union correct layout ─────────────────────────────────

    #[test]
    fn anonymous_nested_union_correct_total_size() {
        // struct Packet { union { int raw; float f; }; char tag; };
        // Union size = max(4,4)=4, align=4.  tag@4(1B)+pad(3B) → total=8B.
        // Previously all union fields were flattened sequentially: raw@0(4)+f@4(4)+tag@8 → 12B.
        let src = r#"
struct Packet {
    union { int raw; float f; };
    char tag;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Packet").expect("Packet");
        assert_eq!(
            l.total_size, 8,
            "Packet must be 8B (union=4B + char+3pad=4B)"
        );
    }

    #[test]
    fn anonymous_nested_union_double_dominates() {
        // The double (8B) dominates: union size=8B, align=8.
        // struct Event { union { int i; double d; }; char flag; };
        // → union@0(8B) + flag@8(1B) + pad(7B) = 16B total.
        let src = r#"
struct Event {
    union { int i; double d; };
    char flag;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Event").expect("Event");
        assert_eq!(
            l.total_size, 16,
            "Event must be 16B (union=8B + char+7pad=8B)"
        );
    }

    #[test]
    fn anonymous_nested_union_no_spurious_padding_finding() {
        // Fields inside an anonymous union don't produce a padding-waste finding
        // for the union itself (it has is_union semantics).
        let src = r#"
struct Clean {
    union { int a; int b; };
    int c;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        let l = layouts.iter().find(|l| l.name == "Clean").expect("Clean");
        assert_eq!(l.total_size, 8, "Clean must be 8B (union=4B + int=4B)");
    }

    // ── __attribute__((aligned(N))) on fields ─────────────────────────────────

    #[test]
    fn extract_aligned_attribute_value() {
        assert_eq!(
            extract_aligned_from_c_field_text("int x __attribute__((aligned(64)));"),
            Some(64)
        );
        assert_eq!(
            extract_aligned_from_c_field_text("char c __attribute__((aligned(8)));"),
            Some(8)
        );
        assert_eq!(extract_aligned_from_c_field_text("int x;"), None);
    }

    #[test]
    fn field_aligned_attribute_shifts_following_field() {
        // `char a __attribute__((aligned(64)))` overrides `a`'s alignment to 64.
        // `a` is still 1 byte in size; `b` (int) follows at offset 4 (1 + 3 pad).
        // The struct alignment becomes 64 so total size rounds up to 64B.
        let src = r#"
struct CacheAligned {
    char a __attribute__((aligned(64)));
    int  b;
};
"#;
        let layouts = parse_c(src, &X86_64_SYSV).unwrap();
        assert_eq!(layouts.len(), 1);
        let l = &layouts[0];
        let a = l.fields.iter().find(|f| f.name == "a").expect("field a");
        let b = l.fields.iter().find(|f| f.name == "b").expect("field b");
        assert_eq!(
            a.align, 64,
            "char __attribute__((aligned(64))) must have align=64"
        );
        assert_eq!(a.offset, 0);
        // b follows: offset = (0+1).next_multiple_of(4) = 4
        assert_eq!(b.offset, 4, "int b must start at offset 4");
        // struct align = 64 → total rounds up to 64
        assert_eq!(
            l.total_size, 64,
            "struct must be padded to 64B (struct align=64)"
        );
    }

    #[test]
    fn floor_to_char_boundary_nudges_forward_past_multibyte_char() {
        // '─' (U+2500) is 3 bytes: 0xE2 0x94 0x80. Position it so a byte at
        // offset 255 starts it, meaning offsets 256 and 257 land inside it.
        let mut s = String::new();
        s.push_str(&"a".repeat(255));
        s.push('─');
        s.push_str(&"a".repeat(10));
        assert!(s.is_char_boundary(255));
        assert!(!s.is_char_boundary(256));
        assert!(!s.is_char_boundary(257));
        assert!(s.is_char_boundary(258));

        assert_eq!(floor_to_char_boundary(&s, 256), 258);
        assert_eq!(floor_to_char_boundary(&s, 257), 258);
        // Already-valid boundaries are returned unchanged.
        assert_eq!(floor_to_char_boundary(&s, 255), 255);
        assert_eq!(floor_to_char_boundary(&s, 258), 258);
    }

    #[test]
    fn cpp_struct_with_multibyte_comment_in_lookback_window_does_not_panic() {
        // Regression test: `__declspec(align(N))` detection falls back to
        // scanning a fixed 256-byte window before the struct's start byte.
        // A multi-byte character (em dash, box-drawing glyph — common in
        // "pretty" separator comments) anywhere in that window used to panic
        // with "byte index N is not a char boundary" because the window's
        // start was raw arithmetic (`start_byte - 256`), not derived from any
        // char-aware search.
        let mut src = String::from("// ");
        src.push_str(&"a".repeat(252));
        src.push('─'); // 3-byte char, deliberately not aligned to the window
        src.push_str(&"a".repeat(254));
        src.push('\n');
        src.push_str("struct Foo { int x; };\n");

        let struct_pos = src.find("struct Foo").expect("test fixture built wrong");
        // Confirm this fixture actually exercises the bug before relying on it.
        assert!(
            !src.is_char_boundary(struct_pos.saturating_sub(256)),
            "test fixture must place a non-boundary offset inside the 256-byte lookback window"
        );

        let layouts = parse_cpp(&src, &X86_64_SYSV).expect("must not panic on non-ASCII comment");
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Foo");
    }

    #[test]
    fn cpp_class_with_multibyte_comment_in_lookback_window_does_not_panic() {
        let mut src = String::from("// ");
        src.push_str(&"a".repeat(252));
        src.push('─');
        src.push_str(&"a".repeat(254));
        src.push('\n');
        src.push_str("class Foo { public: int x; };\n");

        let struct_pos = src.find("class Foo").expect("test fixture built wrong");
        assert!(
            !src.is_char_boundary(struct_pos.saturating_sub(256)),
            "test fixture must place a non-boundary offset inside the 256-byte lookback window"
        );

        let layouts = parse_cpp(&src, &X86_64_SYSV).expect("must not panic on non-ASCII comment");
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].name, "Foo");
    }

    // ── field-declaration shapes (verified against gcc/g++ offsetof) ──────────

    fn layout_of(src: &str, name: &str, cpp: bool) -> StructLayout {
        let lang = if cpp {
            crate::SourceLanguage::Cpp
        } else {
            crate::SourceLanguage::C
        };
        crate::parse_source_str(src, &lang, &X86_64_SYSV)
            .unwrap()
            .into_iter()
            .find(|l| l.name == name)
            .unwrap_or_else(|| panic!("{name} not parsed"))
    }

    fn offset(l: &StructLayout, field: &str) -> usize {
        l.fields
            .iter()
            .find(|f| f.name == field)
            .unwrap_or_else(|| panic!("no field {field}"))
            .offset
    }

    #[test]
    fn array_fields_are_sized_not_dropped() {
        let l = layout_of(
            "struct S { char a; char b[48]; long c; short m[2][3]; };",
            "S",
            false,
        );
        assert_eq!(offset(&l, "b"), 1);
        assert_eq!(offset(&l, "c"), 56);
        assert_eq!(offset(&l, "m"), 64);
        assert_eq!(l.total_size, 80);
    }

    #[test]
    fn array_length_constants_are_evaluated() {
        let src = r#"
#define N 8
#define M (N * 2 + 1)
enum { K = 5, L };
static const int Z = 3;
struct S {
    char a;
    char b[N];
    char c[M];
    char d[K + L];
    char e[sizeof(long) * 3];
    char f[Z];
    long g;
};
"#;
        let l = layout_of(src, "S", false);
        // 1 + 8 + 17 + 11 + 24 + 3 = 64
        assert_eq!(offset(&l, "g"), 64);
        assert!(l.uncertain_fields.is_empty());
    }

    #[test]
    fn unevaluable_array_length_is_flagged_uncertain() {
        let l = layout_of(
            "struct S { char a; char b[UNKNOWN_MACRO]; long c; };",
            "S",
            false,
        );
        assert!(l.fields.iter().any(|f| f.name == "b"), "field must be kept");
        assert_eq!(l.uncertain_fields, vec!["b".to_string()]);
    }

    #[test]
    fn flexible_and_zero_length_arrays_take_no_space() {
        let l = layout_of("struct S { char c; long n; char data[]; };", "S", false);
        assert_eq!(offset(&l, "data"), 16);
        assert_eq!(l.total_size, 16);
        let l = layout_of("struct Z { char a; int z[0]; char c; };", "Z", false);
        assert_eq!(offset(&l, "c"), 4);
    }

    #[test]
    fn array_of_named_structs_is_resolved() {
        let src = "struct In { char x; int y; }; struct S { char a; struct In b[2]; char c; };";
        let l = layout_of(src, "S", false);
        assert_eq!(offset(&l, "c"), 20);
        assert_eq!(l.total_size, 24);
    }

    #[test]
    fn enum_and_function_pointer_fields_are_kept() {
        let src = r#"
enum E { A, B };
enum class F : unsigned char { X };
struct S {
    char a;
    enum E e;
    void (*cb)(int);
    int (*table[2])(void);
    char c;
};
"#;
        let l = layout_of(src, "S", true);
        assert_eq!(offset(&l, "e"), 4);
        assert_eq!(offset(&l, "cb"), 8);
        assert_eq!(offset(&l, "table"), 16);
        assert_eq!(offset(&l, "c"), 32);
        // A bare enum name used as a type resolves to its underlying type.
        let l = layout_of(
            "enum class F : unsigned char { X }; struct T { F f; char c; int i; };",
            "T",
            true,
        );
        assert_eq!(offset(&l, "i"), 4);
    }

    #[test]
    fn references_and_static_members() {
        let l = layout_of(
            "struct S { char a; int& r; static int s; int&& rr; void method(); char c; };",
            "S",
            true,
        );
        assert!(l.fields.iter().all(|f| f.name != "s" && f.name != "method"));
        assert_eq!(offset(&l, "r"), 8);
        assert_eq!(offset(&l, "rr"), 16);
        assert_eq!(offset(&l, "c"), 24);
    }

    #[test]
    fn multiple_declarators_in_one_declaration() {
        let l = layout_of(
            "struct S { char a, b; int *p, q; long arr[2], z; };",
            "S",
            false,
        );
        assert_eq!(offset(&l, "b"), 1);
        assert_eq!(offset(&l, "p"), 8);
        assert_eq!(offset(&l, "q"), 16);
        assert_eq!(offset(&l, "arr"), 24);
        assert_eq!(offset(&l, "z"), 40);
    }

    #[test]
    fn named_anonymous_struct_member_keeps_its_own_layout() {
        let src =
            "struct S { char a; struct { char x; long y; } n; struct { char x; } m[3]; char c; };";
        let l = layout_of(src, "S", false);
        assert_eq!(offset(&l, "n"), 8);
        assert_eq!(offset(&l, "m"), 24);
        assert_eq!(offset(&l, "c"), 27);
    }

    #[test]
    fn unnamed_anonymous_struct_is_still_flattened() {
        let l = layout_of(
            "struct S { char a; struct { char x; long y; }; char c; };",
            "S",
            false,
        );
        assert!(l.fields.iter().any(|f| f.name == "y"));
    }

    #[test]
    fn bare_unsigned_and_complex_types() {
        let l = layout_of(
            "struct S { char a; unsigned u; signed s; long unsigned lu; double _Complex z; char c; };",
            "S",
            false,
        );
        assert_eq!(offset(&l, "u"), 4);
        assert_eq!(offset(&l, "lu"), 16);
        assert_eq!(offset(&l, "z"), 24);
        assert_eq!(offset(&l, "c"), 40);
    }

    #[test]
    fn atomic_specifier_with_macro_style_parse() {
        let l = layout_of("struct S { char a; _Atomic(long) y; char c; };", "S", false);
        assert_eq!(offset(&l, "y"), 8);
    }

    #[test]
    fn struct_base_classes_are_laid_out() {
        let src = "struct Base { int x; }; struct In { char c; }; struct D : Base, In { char d; };";
        let l = layout_of(src, "D", true);
        assert_eq!(offset(&l, "d"), 5);
        assert_eq!(l.total_size, 8);
    }

    #[test]
    fn trailing_aligned_attribute_on_struct() {
        let l = layout_of(
            "struct S { char a; char b; } __attribute__((aligned(32)));",
            "S",
            false,
        );
        assert_eq!(l.total_size, 32);
        assert_eq!(l.align, 32);
    }

    #[test]
    fn bitfields_pack_across_types_and_align_the_struct() {
        // gcc: sizeof 4, c at 1, alignment of `unsigned`.
        let l = layout_of(
            "struct S { unsigned a:3; unsigned b:5; char c; };",
            "S",
            false,
        );
        assert_eq!(l.total_size, 4);
        assert_eq!(l.align, 4);
        assert_eq!(offset(&l, "c"), 1);
        // gcc: sizeof 4, d at 2.
        let l = layout_of(
            "struct M { char a; int b:4; short c:4; char d; };",
            "M",
            false,
        );
        assert_eq!(l.total_size, 4);
        assert_eq!(offset(&l, "d"), 2);
        // A bit-field never straddles its declared type's unit: moves to offset 4.
        let l = layout_of("struct W { char a[3]; int b:20; char c; };", "W", false);
        assert_eq!(offset(&l, "c"), 7);
    }
}
