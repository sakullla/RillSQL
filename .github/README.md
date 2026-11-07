# CI and releases

CI runs Rust tests and Clippy on Windows, macOS, and Linux. It does not build a Vue frontend or a WebView. Browser synthetic IPC is not part of this pipeline and does not replace native database verification.

On a main push, three cache jobs prepare release-profile Rust dependencies when their cache is missing. Their `release-v1-${target}` shared keys and workspace configuration match the release workflow. Tags read main's caches and do not save tag-scoped copies. The first main run after a toolchain or dependency change can take longer; subsequent releases reuse those compiled dependencies. Application crates are rebuilt for each release.

A tag release waits for the latest `push` main CI run for its exact commit, and checks that the commit is still on main. It does not rerun the full CI suite. All build jobs check out that immutable SHA, and an existing draft can only be resumed if its target SHA matches. Before publication, the workflow checks the draft flag, prerelease flag, target SHA, and exactly one nonempty uploaded installer with each expected version and architecture filename. It then resolves the remote tag again to reject a moved tag.

Use the sequence in `AGENTS.md`: validate, push the release branch and main, wait for main CI, then push an annotated version tag. Rerun transient infrastructure failures on the same commit. For code fixes, increment the patch version; never move a pushed release tag. Published releases cannot be overwritten by this workflow.

Run the release guard tests locally with `node --test .github/scripts/release.test.mjs`. They cover manifest consistency, asset validation, and the main CI gate (including wrong commits, PR runs, failed newer runs and commits outside main).

## macOS signing and first launch

`release.yml` packages macOS with cargo-packager and does not call `sign-macos-bundles.sh` or `verify-macos-bundles.sh`. Those scripts target the old Tauri bundle layout. Main CI still runs `test-macos-signing.sh` against that script's fixtures; the smoke test is not a DMG built from the GPUI binary. The fixed self-signed certificate secrets remain in the repository, but this workflow does not apply them to the cargo-packager app or DMG.

The persistent identity is `RillSQL Code Signing`, stored in repository secrets `APPLE_CERTIFICATE` (base64 PKCS#12), `APPLE_CERTIFICATE_PASSWORD`, and `APPLE_SIGNING_IDENTITY`. These were configured with `gh secret set`; preserve the protected backup outside the repository at `~/.config/rillsql/signing/`. Reuse this certificate across releases; do not generate a new key in CI. It is independent of the minisign updater key. Do not modify an archive after generating its updater signature.

The signing script registers its temporary keychain in the user search list and as the default, resolves the imported certificate's SHA-1 identity, and confirms that its private key is available before signing. It restores the previous keychain settings on exit. Main CI runs a native signing smoke test on the same macOS runner image as releases, including all three bundle copies, resource-tampering rejection, and keychain restoration. This catches signing environment failures before a release tag is pushed. The v0.2.10 draft stopped at `codesign: no identity found`; its tag is preserved and the fix uses a new patch version.

The fixed self-signed certificate seals the bundle and preserves signing identity across releases, but does not identify an Apple-trusted developer or provide notarization. Browser downloads can still be blocked by Gatekeeper. After moving RillSQL.app to Applications, try opening it and use System Settings → Privacy & Security → Open Anyway when available. If macOS reports “damaged” without that option, users who trust the download from this repository's Release can remove only this app's quarantine attribute:

```sh
xattr -dr com.apple.quarantine "/Applications/RillSQL.app"
open "/Applications/RillSQL.app"
```

Replace the path if installed elsewhere. Do not disable Gatekeeper globally. Ad-hoc signing is not Apple Developer ID signing or notarization.

The v0.2.9 DMG was verified against its GitHub asset SHA-256 but failed bundle signature verification: its executable had only a linker-generated signature, with no sealed resources or bound Info.plist. Existing v0.2.9 installations from that trusted release also need a one-time bundle signature repair before opening:

```sh
xattr -dr com.apple.quarantine "/Applications/RillSQL.app"
codesign --force --sign - "/Applications/RillSQL.app"
codesign --verify --deep --strict --verbose=2 "/Applications/RillSQL.app"
open "/Applications/RillSQL.app"
```

This repair is specific to the verified v0.2.9 packaging defect; do not use re-signing to conceal unexplained integrity failures in other downloads. Published assets and tags stay unchanged; the build fix ships in a new version.

## cargo-packager

`.github/workflows/package-rust.yml` is a manual packaging check for cargo-packager 0.11.8. It is not part of `ci.yml` and it does not publish a GitHub Release. The Windows job can build NSIS; macOS builds app/DMG and Linux builds DEB/AppImage only when that workflow is run on those runners. Its complete job installs Linux dependencies, then runs `rillsql-package --require-complete` and fails unless the Windows NSIS, macOS DMG, Linux DEB, AppImage, macOS updater archive and `.sig` files are all nonempty and use the existing `RillSQL_<version>_<arch>` names. Missing any of them means the version is not complete. `release.yml` publishes with the same packager and still refuses to publish without those assets.

## In-app updates

Starting with v0.2.4, Settings → App updates supports manual checks, an optional startup check (enabled by default), download progress, and explicit installation/restart. Startup checks only notify; they never install automatically. Installation refuses active queries, export/import work and unsaved table/structure edits, then checkpoints SQL drafts and the active tab before the installer can exit the application. The same checks run again after download. Older versions without the updater must install v0.2.4 manually once.

The GPUI client verifies package signatures with `UPDATER_PUBKEY` in `src-tauri/src/release/mod.rs` and requires the signed version to match the announced release. The independent RillSQL signing key and password are configured as `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets; keep their secure backup outside the repository. Preserve this key for future releases so installed clients continue to trust updates. These updater signatures are separate from Windows/macOS platform code signing. `latest.json` has no `format` field.

Matrix jobs upload installers, the macOS `.app.tar.gz` update archive, and their `.sig` files. They do not upload competing partial manifests. The single publish job checks all four update packages and signature metadata (key ID and bound version), generates all seven platform/package aliases with immutable tag URLs, and uploads `latest.json` before publishing. Cryptographic package verification is performed by the client updater. Linux DEB and AppImage installations receive their corresponding format; macOS supports ARM64 and Windows supports x64. Stable clients follow the latest stable GitHub release.

Browser verification uses synthetic IPC and cannot prove native installation/relaunch. A full upgrade smoke test requires two signed native versions; do not treat mocked install tests as that evidence.

The macOS release job requests cargo-packager formats `app,dmg` so the updater archive can be built from the app. The unpublished v0.2.3 draft records the initial missing-archive failure; its tag is preserved.
