// padlock-cli/src/config.rs
//
// Reads and applies per-project configuration from `.padlock.toml`.
//
// padlock looks for the config file by walking up from the analysed file's
// directory to the filesystem root, stopping at the first `.padlock.toml`
// found. This mirrors how tools like rustfmt and clippy locate their configs.
//
// Example `.padlock.toml`:
//
//   [padlock]
//   min_severity   = "medium"    # report only medium and above (high|medium|low)
//   fail_below     = 60          # exit 1 if any struct scores below this
//   ignore         = ["GeneratedStruct", "FfiLayout"]  # suppress by name
//
//   [arch]
//   override = "aarch64"         # force a specific arch (x86_64|aarch64|aarch64_apple|wasm32|riscv64)
//
//   # Per-struct overrides — keyed by exact struct name
//   [ignore."MyFfiStruct"]       # suppress entirely (same as adding to `ignore` list)
//   [override."HotPath"]
//   min_severity = "high"        # only report High findings for this struct
//   fail_below   = 50            # lower threshold for this struct

use std::path::{Path, PathBuf};

use padlock_core::findings::Severity;

const CONFIG_FILENAME: &str = ".padlock.toml";

/// Per-struct severity and threshold overrides.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructOverride {
    /// Override min_severity for this struct only.
    pub min_severity: Option<Severity>,
    /// Override fail_below for this struct only.
    pub fail_below: Option<u8>,
}

/// Loaded and validated project configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Minimum severity to report. Findings below this level are suppressed.
    pub min_severity: Severity,
    /// Exit non-zero if any struct's score falls below this value (0 = disabled).
    pub fail_below: u8,
    /// Struct names to suppress entirely from output and exit-code logic.
    pub ignore: Vec<String>,
    /// Optional architecture override name (validated at load time).
    pub arch_override: Option<String>,
    /// Per-struct overrides keyed by exact struct name.
    pub struct_overrides: std::collections::HashMap<String, StructOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_severity: Severity::Low,
            fail_below: 0,
            ignore: Vec::new(),
            arch_override: None,
            struct_overrides: std::collections::HashMap::new(),
        }
    }
}

impl Config {
    /// Load config by searching upward from `start_dir`.
    /// Returns `Config::default()` if no config file is found.
    pub fn load_from(start_dir: &Path) -> Self {
        find_config_file(start_dir)
            .and_then(|p| Self::load_file(&p))
            .unwrap_or_default()
    }

    /// Load config for a given analysis target path (file or directory).
    #[allow(dead_code)]
    pub fn for_path(target: &Path) -> Self {
        let dir = if target.is_dir() {
            target.to_path_buf()
        } else {
            target.parent().unwrap_or(target).to_path_buf()
        };
        Self::load_from(&dir)
    }

    fn load_file(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let doc: toml::Value = toml::from_str(&text)
            .map_err(|e| eprintln!("padlock: warning: failed to parse {}: {e}", path.display()))
            .ok()?;

        let padlock = doc.get("padlock");
        let arch = doc.get("arch");

        let min_severity = padlock
            .and_then(|p| p.get("min_severity"))
            .and_then(|v| v.as_str())
            .and_then(parse_severity)
            .unwrap_or(Severity::Low);

        let fail_below = padlock
            .and_then(|p| p.get("fail_below"))
            .and_then(|v| v.as_integer())
            .map(|n| n.clamp(0, 100) as u8)
            .unwrap_or(0);

        let ignore = padlock
            .and_then(|p| p.get("ignore"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let arch_override = arch
            .and_then(|a| a.get("override"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        // Per-struct overrides: [override."StructName"]
        let mut struct_overrides = std::collections::HashMap::new();
        if let Some(overrides_table) = doc.get("override").and_then(|v| v.as_table()) {
            for (struct_name, val) in overrides_table {
                let min_sev = val
                    .get("min_severity")
                    .and_then(|v| v.as_str())
                    .and_then(parse_severity);
                let fail_b = val
                    .get("fail_below")
                    .and_then(|v| v.as_integer())
                    .map(|n| n.clamp(0, 100) as u8);
                if min_sev.is_some() || fail_b.is_some() {
                    struct_overrides.insert(
                        struct_name.clone(),
                        StructOverride {
                            min_severity: min_sev,
                            fail_below: fail_b,
                        },
                    );
                }
            }
        }

        Some(Self {
            min_severity,
            fail_below,
            ignore,
            arch_override,
            struct_overrides,
        })
    }

    /// Returns true if a struct with the given name should be suppressed.
    pub fn is_ignored(&self, struct_name: &str) -> bool {
        self.ignore.iter().any(|n| n == struct_name)
    }

    /// Returns true if a finding with the given severity should be reported.
    pub fn should_report(&self, severity: &Severity) -> bool {
        severity_rank(severity) >= severity_rank(&self.min_severity)
    }

    /// Returns true if a finding with the given severity should be reported
    /// for the named struct, applying any per-struct override.
    #[allow(dead_code)]
    pub fn should_report_for(&self, struct_name: &str, severity: &Severity) -> bool {
        let effective_min = self
            .struct_overrides
            .get(struct_name)
            .and_then(|o| o.min_severity.as_ref())
            .unwrap_or(&self.min_severity);
        severity_rank(severity) >= severity_rank(effective_min)
    }

    /// Returns the effective fail_below threshold for the named struct.
    #[allow(dead_code)]
    pub fn fail_below_for(&self, struct_name: &str) -> u8 {
        self.struct_overrides
            .get(struct_name)
            .and_then(|o| o.fail_below)
            .unwrap_or(self.fail_below)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn find_config_file(start: &Path) -> Option<PathBuf> {
    let mut dir = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        let candidate = dir.join(CONFIG_FILENAME);
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn parse_severity(s: &str) -> Option<Severity> {
    match s.to_ascii_lowercase().as_str() {
        "high" => Some(Severity::High),
        "medium" | "med" => Some(Severity::Medium),
        "low" => Some(Severity::Low),
        _ => {
            eprintln!("padlock: warning: unknown min_severity '{s}', using 'low'");
            None
        }
    }
}

fn severity_rank(s: &Severity) -> u8 {
    match s {
        Severity::Low => 0,
        Severity::Medium => 1,
        Severity::High => 2,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn default_config_is_permissive() {
        let cfg = Config::default();
        assert_eq!(cfg.min_severity, Severity::Low);
        assert_eq!(cfg.fail_below, 0);
        assert!(cfg.ignore.is_empty());
    }

    #[test]
    fn parse_full_config() {
        // Write to a temp file then load via load_file
        let content = r#"
[padlock]
min_severity = "medium"
fail_below   = 60
ignore       = ["GeneratedFoo", "FfiLayout"]

[arch]
override = "aarch64"
"#;
        let f = write_config(content);
        let cfg = Config::load_file(f.path()).unwrap();
        assert_eq!(cfg.min_severity, Severity::Medium);
        assert_eq!(cfg.fail_below, 60);
        assert_eq!(cfg.ignore, vec!["GeneratedFoo", "FfiLayout"]);
        assert_eq!(cfg.arch_override.as_deref(), Some("aarch64"));
    }

    #[test]
    fn parse_high_severity() {
        let content = "[padlock]\nmin_severity = \"high\"\n";
        let f = write_config(content);
        let cfg = Config::load_file(f.path()).unwrap();
        assert_eq!(cfg.min_severity, Severity::High);
    }

    #[test]
    fn parse_low_severity() {
        let content = "[padlock]\nmin_severity = \"low\"\n";
        let f = write_config(content);
        let cfg = Config::load_file(f.path()).unwrap();
        assert_eq!(cfg.min_severity, Severity::Low);
    }

    #[test]
    fn missing_keys_use_defaults() {
        let content = "[padlock]\n";
        let f = write_config(content);
        let cfg = Config::load_file(f.path()).unwrap();
        assert_eq!(cfg.min_severity, Severity::Low);
        assert_eq!(cfg.fail_below, 0);
        assert!(cfg.ignore.is_empty());
    }

    #[test]
    fn fail_below_clamped_to_100() {
        let content = "[padlock]\nfail_below = 200\n";
        let f = write_config(content);
        let cfg = Config::load_file(f.path()).unwrap();
        assert_eq!(cfg.fail_below, 100);
    }

    #[test]
    fn is_ignored_matches_exact_name() {
        let cfg = Config {
            ignore: vec!["FfiLayout".into()],
            ..Config::default()
        };
        assert!(cfg.is_ignored("FfiLayout"));
        assert!(!cfg.is_ignored("FfiLayoutExtra"));
    }

    #[test]
    fn should_report_high_always_when_min_low() {
        let cfg = Config::default(); // min_severity = Low
        assert!(cfg.should_report(&Severity::High));
        assert!(cfg.should_report(&Severity::Medium));
        assert!(cfg.should_report(&Severity::Low));
    }

    #[test]
    fn should_report_suppresses_low_when_min_medium() {
        let cfg = Config {
            min_severity: Severity::Medium,
            ..Config::default()
        };
        assert!(cfg.should_report(&Severity::High));
        assert!(cfg.should_report(&Severity::Medium));
        assert!(!cfg.should_report(&Severity::Low));
    }

    #[test]
    fn should_report_only_high_when_min_high() {
        let cfg = Config {
            min_severity: Severity::High,
            ..Config::default()
        };
        assert!(cfg.should_report(&Severity::High));
        assert!(!cfg.should_report(&Severity::Medium));
        assert!(!cfg.should_report(&Severity::Low));
    }

    #[test]
    fn find_config_file_returns_none_for_nonexistent_dir() {
        let result = find_config_file(Path::new("/tmp/__padlock_no_such_dir__"));
        assert!(result.is_none());
    }

    #[test]
    fn load_from_nonexistent_dir_returns_default() {
        let cfg = Config::load_from(Path::new("/tmp/__padlock_no_such_dir__"));
        assert_eq!(cfg, Config::default());
    }
}
