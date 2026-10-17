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
- `cargo test --manifest-path src-tauri/Cargo.toml`: run Rust tests.
- `docker compose -f docker/compose.yaml up -d`: start local PostgreSQL, MySQL, and MariaDB fixtures.

## Coding Style & Naming Conventions
Match existing code: two-space indentation, double quotes, and semicolons in TypeScript; four-space indentation in Rust. Use Vue `<script setup lang="ts">`, PascalCase component filenames, camelCase TypeScript identifiers, and snake_case Rust functions/modules. Keep TypeScript strict-mode compatible. No dedicated frontend formatter or lint script is configured; preserve surrounding style and avoid unrelated formatting changes.

## Testing Guidelines
Frontend tests use Vitest, jsdom, and Vue Test Utils; name them `*.test.ts`. Rust tests typically live in feature `tests.rs` modules. Add focused regression tests for behavior changes; no coverage threshold is configured. For UI changes, install Chromium with `npx playwright install chromium`, start Vite, then run `npm run verify:workbench` and `npm run capture:ui`. These use synthetic IPC; verify native behavior separately in Tauri.

## Commit & Pull Request Guidelines
Recent history commonly uses `feat:`, `fix:`, and `docs:` prefixes with short English or Chinese descriptions. Follow that pattern. PRs should describe the behavior change, link relevant issues, list validation performed, and include screenshots for visual changes. Identify whether evidence comes from synthetic browser fixtures or the native application.
