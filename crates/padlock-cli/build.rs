fn main() {
    // Embed the short git SHA at compile time so `padlock --version` is traceable.
    // Falls back to "unknown" when git is unavailable (e.g. vendored source tarballs).
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BUILD_GIT_SHA={sha}");
    // Re-run when HEAD moves (commit, checkout) or a ref changes.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
}
