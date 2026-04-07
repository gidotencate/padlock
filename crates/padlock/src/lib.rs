// padlock — facade crate
//
// Re-exports the compile-time proc-macro assertions from `padlock-macros` so
// users only need a single dependency:
//
//   [dependencies]
//   padlock = "0.1"
//
// Then in Rust source:
//
//   use padlock::assert_no_padding;
//
//   #[assert_no_padding]
//   struct WellOrdered {
//       a: u64,
//       b: u32,
//       c: u32,
//   }
//
//   use padlock::assert_size;
//
//   #[assert_size(16)]
//   struct ExactlySixteenBytes {
//       a: u64,
//       b: u64,
//   }

pub use padlock_macros::assert_no_padding;
pub use padlock_macros::assert_size;

#[cfg(test)]
mod tests {
    // Smoke-test that the re-exports are available and work via this crate.

    use padlock_macros::assert_no_padding;
    use padlock_macros::assert_size;

    #[allow(dead_code)]
    #[assert_no_padding]
    struct WellOrdered {
        a: u64,
        b: u32,
        c: u32,
    }

    #[allow(dead_code)]
    #[assert_size(16)]
    struct ExactlySixteen {
        a: u64,
        b: u64,
    }

    #[test]
    fn reexports_compile_and_sizes_are_correct() {
        assert_eq!(std::mem::size_of::<WellOrdered>(), 16);
        assert_eq!(std::mem::size_of::<ExactlySixteen>(), 16);
    }
}
