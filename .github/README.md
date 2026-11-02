# CI and releases

CI builds the frontend once, then shares `frontend-dist` with the production-browser checks and the three native Rust test/Clippy jobs. Browser checks use synthetic IPC; they do not replace native database verification.

On a main push, three cache jobs prepare release-profile Rust dependencies when their cache is missing. Their `release-v1-${target}` shared keys and workspace configuration match the release workflow. Tags read main's caches and do not save tag-scoped copies. The first main run after a toolchain or dependency change can take longer; subsequent releases reuse those compiled dependencies. Application crates are rebuilt for each release.

A tag release waits for the latest `push` main CI run for its exact commit, and checks that the commit is still on main. It does not rerun the full CI suite. All build jobs check out that immutable SHA, and an existing draft can only be resumed if its target SHA matches. Before publication, the workflow checks the draft flag, prerelease flag, target SHA, and exactly one nonempty uploaded installer with each expected version and architecture filename. It then resolves the remote tag again to reject a moved tag.

Use the sequence in `AGENTS.md`: validate, push the release branch and main, wait for main CI, then push an annotated version tag. Rerun transient infrastructure failures on the same commit. For code fixes, increment the patch version; never move a pushed release tag. Published releases cannot be overwritten by this workflow.

Run the release guard tests locally with `node --test .github/scripts/release.test.mjs`. They cover manifest consistency, asset validation, and the main CI gate (including wrong commits, PR runs, failed newer runs and commits outside main).

## In-app updates

Starting with v0.2.3, Settings ¡ú App updates supports manual checks, an optional startup check (enabled by default), download progress, and explicit installation/restart. Startup checks only notify; they never install automatically. Installation refuses active queries, export/import work and unsaved table/structure edits, then checkpoints SQL drafts and the active tab before the installer can exit the application. The same checks run again after download. Older versions without the updater must install v0.2.3 manually once.

Tauri verifies package signatures with the public key in `src-tauri/tauri.conf.json` and requires the signed version to match the announced release. The independent RillSQL signing key and password are configured as `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets; keep their secure backup outside the repository. Preserve this key for future releases so installed clients continue to trust updates. These updater signatures are separate from Windows/macOS platform code signing.

Matrix jobs upload installers, the macOS `.app.tar.gz` update archive, and their `.sig` files. They do not upload competing partial manifests. The single publish job checks all four update packages and signature metadata (key ID and bound version), generates all seven platform/package aliases with immutable tag URLs, and uploads `latest.json` before publishing. Cryptographic package verification is performed by the client updater. Linux DEB and AppImage installations receive their corresponding format; macOS supports ARM64 and Windows supports x64. Stable clients follow the latest stable GitHub release.

Browser verification uses synthetic IPC and cannot prove native installation/relaunch. A full upgrade smoke test requires two signed native versions; do not treat mocked install tests as that evidence.
