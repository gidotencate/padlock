# Publishing padlock

How a release is cut. Only the maintainer can do the publishing steps: they need credentials that are not in this repository (a crates.io API token, a VS Code Marketplace publisher account, write access to the GitHub repository). This page documents the process, it does not grant any of it. Anyone is free to fork padlock under the licence and publish their own build under their own name.

## Licence

padlock is dual-licensed under **MIT OR Apache-2.0** (the user's choice), as the Rust compiler is.

- **MIT** — permissive, short, widely understood.
- **Apache-2.0** — adds an explicit patent grant, which matters for enterprise users.

## Who can do what

| Action | Needs |
|---|---|
| Push a `v*` tag (starts the Release workflow: binaries, `.vsix`, GitHub release) | write access to the repository |
| `cargo publish` | a crates.io token of an owner of the `padlock-*` crates |
| Publish the extension | the Marketplace publisher account (and, for Open VSX, its token) |

Keep tokens out of the repository and out of CI logs; the workflows only use the built-in `GITHUB_TOKEN`.

## Version bump checklist

Three files change together (the crates inherit the workspace version):

```
Cargo.toml                       # [workspace.package] version, and the four padlock-* versions in [workspace.dependencies]
editors/vscode/package.json      # "version"
editors/vscode/package-lock.json # "version" in two places (top level and packages[""])
```

After a release, `Formula/padlock.rb` (Homebrew) gets the new `version`, the four download URLs and their `sha256` values, copied from the release assets (`shasum -a 256` of each tarball).

Add a new `## [x.y.z]` section to `CHANGELOG.md` for every version bump; never rename or merge an older one. Then:

```bash
cargo build
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

Layout changes also run `python3 tests/conformance/run.py` (and `--cross` when the toolchains are installed).

## Release flow

```bash
# 1. Work on a branch; update CHANGELOG.md, bump the versions (above).

# 2. Verify
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
python3 tests/conformance/run.py
cd editors/vscode && npm run compile && npx vsce package && cd ../..

# 3. Merge to main (PR with real CI, or a local merge by the maintainer), wait for CI to be green.

# 4. Tag main. release.yml builds the binaries (padlock, cargo-padlock, padlock-lsp)
#    for each platform and the .vsix, and attaches them to a GitHub release.
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z

# 5. Publish the crates, in dependency order, ~30 s apart for the index to catch up.
cargo publish -p padlock-core
cargo publish -p padlock-macros
cargo publish -p padlock-dwarf
cargo publish -p padlock-source
cargo publish -p padlock-output
cargo publish -p padlock-cli      # installs padlock and cargo-padlock
cargo publish -p padlock-lsp      # installs padlock-lsp

# 6. Update Formula/padlock.rb from the release assets, then publish the extension (below).
```

`cargo publish --dry-run -p <crate>` checks the metadata first. Users install with `cargo install padlock-cli`.

## VS Code extension

```bash
cd editors/vscode
npm ci
npm run compile          # tsc -p ./
npx vsce package         # produces padlock-x.y.z.vsix
code --install-extension padlock-x.y.z.vsix   # local test
```

Listing: `https://marketplace.visualstudio.com/items?itemName=gidotencate.padlock`

**Publishing with a PAT is currently broken.** The Marketplace's Azure DevOps token auth rejects valid tokens (an Azure AD problem on Microsoft's side), so `release.yml` builds and attaches the `.vsix` but does not run `vsce publish`. Publish by hand instead:

1. Download the `.vsix` from the tagged GitHub release (or build it as above).
2. Open the [publisher dashboard](https://marketplace.visualstudio.com/manage) → padlock → **Update** → upload the `.vsix`. This authenticates with your browser session, not a PAT.

Re-check whether PAT auth works again; if it does, add a `vsce publish --pat ${{ secrets.VSCE_PAT }}` step to the `build-vscode-extension` job.

### Open VSX (VSCodium, Theia)

```bash
npx ovsx publish padlock-x.y.z.vsix --pat <open-vsx-token>
```
