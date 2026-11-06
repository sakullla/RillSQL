# Repository Guidelines

## Project Structure & Module Organization
RillSQL is a Vue 3/TypeScript database workbench with a Tauri 2/Rust backend. Frontend features live in `src/` (`connections`, `catalog`, `query`, `grid`, `structure`, `export`, `history`); shared application UI lives in `src/shell/`, with styles in `src/styles/`. Rust modules live in `src-tauri/src/`; native permissions, configuration, and icons are under `src-tauri/`. Tests sit beside their features. `docker/` contains development databases and seed SQL; `scripts/` contains browser verification utilities; `docs/verification/` stores reports and screenshots. Treat `dist/` and `src-tauri/target/` as generated output.

## Build, Test, and Development Commands
Run commands from the repository root:

- `npm ci`: install dependencies from the lockfile.
- `npm run dev`: start Vite at `http://localhost:1420`.
- `npm run tauri -- dev`: launch the desktop app with the development server; requires Rust 1.90+ and platform Tauri prerequisites.
- `npm run typecheck`: check Vue and TypeScript types.
- `npm run build`: type-check and build the frontend.
- `npm run tauri -- build`: build and bundle the desktop application.
- `npm test`: run the frontend test suite once.
- `cargo test --manifest-path src-tauri/Cargo.toml`: run Rust tests; run `npm run build` first because `tauri::generate_context!` embeds `dist/`.
- `docker compose -f docker/compose.yaml up -d`: start local PostgreSQL, MySQL, and MariaDB fixtures.

## Coding Style & Naming Conventions
Match existing code: two-space indentation, double quotes, and semicolons in TypeScript; four-space indentation in Rust. Use Vue `<script setup lang="ts">`, PascalCase component filenames, camelCase TypeScript identifiers, and snake_case Rust functions/modules. Keep TypeScript strict-mode compatible. No dedicated frontend formatter or lint script is configured; preserve surrounding style and avoid unrelated formatting changes.

## Testing Guidelines
Frontend tests use Vitest, jsdom, and Vue Test Utils; name them `*.test.ts`. Rust tests typically live in feature `tests.rs` modules. Add focused regression tests for behavior changes; no coverage threshold is configured. For UI changes, install Chromium with `npx playwright install chromium`, start Vite, then run `npm run verify:workbench` and `npm run capture:ui`. These use synthetic IPC; verify native behavior separately in Tauri.

## Commit & Pull Request Guidelines
Recent history commonly uses `feat:`, `fix:`, and `docs:` prefixes with short English or Chinese descriptions. Follow that pattern. PRs should describe the behavior change, link relevant issues, list validation performed, and include screenshots for visual changes. Identify whether evidence comes from synthetic browser fixtures or the native application.

## CI
`.github/workflows/ci.yml` runs on pull requests and pushes to `main`: frontend checks, browser verification of the production build with synthetic IPC (screenshots are uploaded as the `ui-capture` artifact), and Rust tests plus Clippy on Windows, macOS, and Linux. The frontend build is shared between jobs. Main pushes also seed release-profile Rust dependency caches on cache misses.

## Release Process
Versions follow SemVer. `.github/workflows/release.yml` runs when a `v*` tag is pushed: it requires successful main CI for that exact commit, creates a draft release with notes generated from commit subjects, builds the Windows (NSIS), macOS ARM64 (DMG), and Linux (DEB, AppImage) installers, verifies their exact version/architecture filenames and nonempty uploads, then publishes the draft. Every checkout and draft is bound to the verified commit SHA; publication checks the tag has not moved. Updater packages are signed with a dedicated RillSQL key and published with a complete `latest.json` after all platform builds finish. macOS release apps use a fixed self-signed certificate without Apple Developer ID signing or notarization; Windows installers are not platform code-signed. The macOS build verifies bundle signatures in the app, DMG and updater archive before publication. See `.github/README.md` for cache and retry details.

1. On an up-to-date `main`, set the same version in all six places; `node .github/scripts/release.mjs check` fails on any mismatch:
   - `package.json`
   - `package-lock.json` (top-level `version` and `packages[""].version`)
   - `src-tauri/Cargo.toml`
   - `src-tauri/Cargo.lock` (the `rillsql` package)
   - `src-tauri/tauri.conf.json`
   - `PRODUCT_VERSION` in `src/shell/product.ts`
2. Validate locally: `npm test`, `npm run build`, `node scripts/i18n-scan.mjs`, `node --test .github/scripts/release.test.mjs`. For UI changes, also start Vite and run `npm run verify:workbench` and `npm run capture:ui`.
3. Commit as `chore: 发布 vX.Y.Z` on `main` and push it: `git push origin main`. GitHub only registers a workflow once it exists on the default branch; a tag pushed before that starts nothing and reports no error.
4. Wait for CI on `main` to pass: `gh run watch`. The release job depends on the same CI, so a failure here would stop the release anyway.
5. Create and push an annotated tag: `git tag -a vX.Y.Z -m "RillSQL vX.Y.Z"`, then `git push origin vX.Y.Z`. `release.mjs check vX.Y.Z` rejects a tag that does not match the version.
6. Follow the run with `gh run watch` and confirm the release with `gh release view vX.Y.Z`.

For infrastructure failures, rerun the failed jobs on the same tag and commit; the workflow resumes only a matching draft. If code must change, use a new patch version and tag. Do not move a pushed release tag or reuse a published version. A tag containing `-` (for example `v0.2.1-rc.1`) is published as a pre-release.
