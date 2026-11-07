# Repository Guidelines

## Project Structure & Module Organization
RillSQL is a Rust database workbench. The GPUI application lives in `src-tauri/src/` (`app`, `connections`, `catalog`, `query`, `grid`, `structure` via `ddl`, `export`, `history` via `query`). `src-tauri/src/main.rs` starts `app::run()`. Icons and the cargo-packager metadata live under `src-tauri/`. Tests sit beside their features. `docker/` contains development databases and seed SQL. `docs/verification/` stores reports and screenshots. Treat `src-tauri/target/` as generated output.

## Build, Test, and Development Commands
Run commands from the repository root:

- `cargo run --manifest-path src-tauri/Cargo.toml --bin rillsql`: launch the GPUI desktop app. Requires Rust 1.90+.
- `cargo test --manifest-path src-tauri/Cargo.toml --locked`: run the Rust test suite.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`: lint the Rust crate.
- `cargo build --release --locked --manifest-path src-tauri/Cargo.toml --bin rillsql` then `cargo run --manifest-path src-tauri/Cargo.toml --bin rillsql-package --features packager --locked -- --formats nsis`: package with cargo-packager 0.11.8. Do not start the second command until the first exits; it must not call cargo while the target directory lock is held. Windows can produce NSIS. macOS app/DMG and Linux DEB/AppImage are configured, but a release is complete only when all four installers and the signed updater manifest exist.
- `docker compose -f docker/compose.yaml up -d`: start local PostgreSQL, MySQL, and MariaDB fixtures.

## Coding Style & Naming Conventions
Match existing code: four-space indentation in Rust, snake_case functions and modules. Keep the crate free of a crate-level `allow` that would hide Clippy warnings. No dedicated frontend formatter is part of the product build.

## Testing Guidelines
Rust tests typically live in feature `tests.rs` modules. Add focused regression tests for behavior changes; no coverage threshold is configured. UI behavior is covered by the GPUI application and Rust tests. A release still needs native startup evidence on each platform.

## Commit & Pull Request Guidelines
Recent history commonly uses `feat:`, `fix:`, and `docs:` prefixes with short English or Chinese descriptions. Follow that pattern. PRs should describe the behavior change, link relevant issues, list validation performed, and include screenshots for visual changes. Identify whether evidence comes from Rust tests or the native application.

## CI
`.github/workflows/ci.yml` runs on pull requests and pushes to `main`: release-metadata checks, then Rust tests plus Clippy on Windows, macOS, and Linux. Main pushes also seed release-profile Rust dependency caches on cache misses. `.github/workflows/package-rust.yml` is a manual packaging check and does not publish.

## Release Process
Versions follow SemVer. `.github/workflows/release.yml` runs when a `v*` tag is pushed: it requires successful main CI for that exact commit, creates a draft release with notes generated from commit subjects, builds the Windows (NSIS), macOS ARM64 (DMG), and Linux (DEB, AppImage) installers with cargo-packager, verifies their exact version/architecture filenames and nonempty uploads, then publishes the draft. Every checkout and draft is bound to the verified commit SHA; publication checks the tag has not moved. Updater packages are signed with a dedicated RillSQL key and published with a complete `latest.json` after all platform builds finish. A missing platform installer blocks publication. Windows installers are not platform code-signed. See `.github/README.md` for cache and retry details.

1. On an up-to-date `main`, set the same version in both places; `node .github/scripts/release.mjs check` fails on any mismatch:
   - `src-tauri/Cargo.toml`
   - `src-tauri/Cargo.lock` (the `rillsql` package)
2. Validate locally: `cargo test --manifest-path src-tauri/Cargo.toml --locked`, `node --test .github/scripts/release.test.mjs`.
3. Commit as `chore: 发布 vX.Y.Z` on `main` and push it: `git push origin main`. GitHub only registers a workflow once it exists on the default branch; a tag pushed before that starts nothing and reports no error.
4. Wait for CI on `main` to pass: `gh run watch`. The release job depends on the same CI, so a failure here would stop the release anyway.
5. Create and push an annotated tag: `git tag -a vX.Y.Z -m "RillSQL vX.Y.Z"`, then `git push origin vX.Y.Z`. `release.mjs check vX.Y.Z` rejects a tag that does not match the version.
6. Follow the run with `gh run watch` and confirm the release with `gh release view vX.Y.Z`.

For infrastructure failures, rerun the failed jobs on the same tag and commit; the workflow resumes only a matching draft. If code must change, use a new patch version and tag. Do not move a pushed release tag or reuse a published version. A tag containing `-` (for example `v0.2.1-rc.1`) is published as a pre-release.
