fn main() {
    // Embed the short git SHA at compile time so `padlock --version` is traceable.
    //
    // `git rev-parse` only works inside an actual git checkout, which a
    // `cargo install padlock-cli` from crates.io is not — crates.io strips
    // the `.git` directory from published packages. For that case, fall
    // back to `.cargo_vcs_info.json`: `cargo publish` writes it into the
    // package root with the exact commit it was published from, unless
    // published with `--allow-dirty` (no file at all — "unknown" is the
    // correct answer there, since there's genuinely no clean commit to
    // trace to).
    let sha = git_sha()
        .or_else(sha_from_cargo_vcs_info)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BUILD_GIT_SHA={sha}");
    // Re-run when HEAD moves (commit, checkout) or a ref changes.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
}

fn git_sha() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Extracts the short SHA from `.cargo_vcs_info.json` without pulling in a
/// JSON parser for one field: find `"sha1"`, then the first quoted string
/// after its colon.
fn sha_from_cargo_vcs_info() -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let path = std::path::Path::new(&manifest_dir).join(".cargo_vcs_info.json");
    let contents = std::fs::read_to_string(path).ok()?;

    let after_key = contents.split_once("\"sha1\"")?.1;
    let after_colon = after_key.split_once(':')?.1;
    let after_open_quote = after_colon.split_once('"')?.1;
    let sha1 = after_open_quote.split_once('"')?.0;

    (sha1.len() >= 7).then(|| sha1[..7].to_string())
}
