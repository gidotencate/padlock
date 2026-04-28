// padlock-cli/src/commands/watch.rs
//
// `padlock watch <path>` — re-runs analysis whenever the watched file changes.
//
// On change the terminal is cleared and a fresh analysis is printed, giving a
// live feedback loop while editing structs or rebuilding.
//
// Typical use:
//   # Watch a Rust source file while editing
//   padlock watch src/pool.rs
//
//   # Watch a compiled binary (pair with `cargo watch -x build`)
//   padlock watch target/debug/myapp
//
// The watcher uses the `notify` crate (cross-platform: inotify on Linux,
// FSEvents on macOS, ReadDirectoryChangesW on Windows). Directories are
// watched recursively; files are watched directly.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use padlock_core::findings::Report;

/// Minimum interval between re-analysis runs. Debounces rapid successive
/// write events (e.g. editor atomic-saves that produce two events).
const DEBOUNCE: Duration = Duration::from_millis(250);

pub fn run(path: &Path, json: bool) -> anyhow::Result<()> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    eprintln!("padlock watch: watching `{}`", path.display());
    eprintln!("padlock watch: press Ctrl+C to stop\n");

    // Run once immediately on start.
    analyse_and_print(&path, json);

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher: RecommendedWatcher = Watcher::new(
        tx,
        notify::Config::default().with_poll_interval(Duration::from_millis(500)),
    )?;

    let mode = if path.is_dir() {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher.watch(&path, mode)?;

    let mut last_run = Instant::now();

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                if !is_relevant_event(&event) {
                    continue;
                }
                // Debounce: ignore events that arrive within DEBOUNCE of the last run.
                let now = Instant::now();
                if now.duration_since(last_run) < DEBOUNCE {
                    // Drain any queued events but don't re-run yet.
                    continue;
                }
                // Brief sleep to let the write fully flush before reading.
                std::thread::sleep(Duration::from_millis(50));
                clear_terminal();
                eprintln!("padlock watch: change detected — re-analysing…\n");
                analyse_and_print(&path, json);
                last_run = Instant::now();
            }
            Ok(Err(e)) => {
                eprintln!("padlock watch: watcher error: {e}");
            }
            Err(_) => {
                // Channel closed (watcher dropped) — exit.
                break;
            }
        }
    }

    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn is_relevant_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn clear_terminal() {
    // ANSI escape: move cursor to top-left and clear screen.
    // Works on Linux/macOS terminals; no-op on dumb terminals.
    print!("\x1b[2J\x1b[H");
}

fn analyse_and_print(path: &Path, json: bool) {
    match run_analysis(path) {
        Ok(report) => {
            if json {
                match padlock_output::to_json(&report) {
                    Ok(s) => println!("{s}"),
                    Err(e) => eprintln!("padlock watch: JSON error: {e}"),
                }
            } else {
                print!("{}", padlock_output::render_report(&report, false));
            }
        }
        Err(e) => {
            eprintln!("padlock watch: analysis error: {e}");
        }
    }
}

fn run_analysis(path: &Path) -> anyhow::Result<Report> {
    let layouts = if padlock_source::detect_language(path).is_some() {
        let arch = padlock_dwarf::reader::detect_arch_from_host();
        padlock_source::parse_source(path, arch)?.layouts
    } else {
        // Binary path — may not exist yet if the build hasn't finished.
        if !path.exists() {
            anyhow::bail!(
                "`{}` does not exist yet — waiting for build to complete.",
                path.display()
            );
        }
        let data = std::fs::read(path)?;
        let dwarf = padlock_dwarf::reader::load(&data)?;
        let arch =
            padlock_dwarf::reader::detect_arch(&data).unwrap_or(&padlock_core::arch::X86_64_SYSV);
        padlock_dwarf::extractor::Extractor::new(&dwarf, arch).extract_all()?
    };

    Ok(Report::from_layouts(&layouts))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};
    use notify::{Event, EventKind};
    use std::path::PathBuf;

    fn make_event(kind: EventKind) -> Event {
        Event {
            kind,
            paths: vec![],
            attrs: Default::default(),
        }
    }

    #[test]
    fn create_event_is_relevant() {
        let e = make_event(EventKind::Create(CreateKind::File));
        assert!(is_relevant_event(&e));
    }

    #[test]
    fn modify_event_is_relevant() {
        let e = make_event(EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content,
        )));
        assert!(is_relevant_event(&e));
    }

    #[test]
    fn access_event_is_not_relevant() {
        let e = make_event(EventKind::Access(AccessKind::Read));
        assert!(!is_relevant_event(&e));
    }

    #[test]
    fn other_event_is_not_relevant() {
        let e = make_event(EventKind::Other);
        assert!(!is_relevant_event(&e));
    }

    #[test]
    fn debounce_constant_is_reasonable() {
        assert!(
            DEBOUNCE.as_millis() >= 100,
            "debounce too short — rapid saves will cause double-analysis"
        );
        assert!(
            DEBOUNCE.as_millis() <= 2000,
            "debounce too long — changes will feel laggy"
        );
    }

    #[test]
    fn watch_path_canonicalisation_does_not_panic_on_missing_file() {
        let p = PathBuf::from("/tmp/__padlock_nonexistent_test_file__");
        // canonicalize fails; the fallback should be the original path, not a panic.
        let result = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        assert_eq!(result, p);
    }
}
