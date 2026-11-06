# CI and releases

CI builds the frontend once, then shares `frontend-dist` with the production-browser checks and the three native Rust test/Clippy jobs. Browser checks use synthetic IPC; they do not replace native database verification.

On a main push, three cache jobs prepare release-profile Rust dependencies when their cache is missing. Their `release-v1-${target}` shared keys and workspace configuration match the release workflow. Tags read main's caches and do not save tag-scoped copies. The first main run after a toolchain or dependency change can take longer; subsequent releases reuse those compiled dependencies. Application crates are rebuilt for each release.

A tag release waits for the latest `push` main CI run for its exact commit, and checks that the commit is still on main. It does not rerun the full CI suite. All build jobs check out that immutable SHA, and an existing draft can only be resumed if its target SHA matches. Before publication, the workflow checks the draft flag, prerelease flag, target SHA, and exactly one nonempty uploaded installer with each expected version and architecture filename. It then resolves the remote tag again to reject a moved tag.

Use the sequence in `AGENTS.md`: validate, push the release branch and main, wait for main CI, then push an annotated version tag. Rerun transient infrastructure failures on the same commit. For code fixes, increment the patch version; never move a pushed release tag. Published releases cannot be overwritten by this workflow.

Run the release guard tests locally with `node --test .github/scripts/release.test.mjs`. They cover manifest consistency, asset validation, and the main CI gate (including wrong commits, PR runs, failed newer runs and commits outside main).

## macOS signing and first launch

`bundle.macOS.signingIdentity` is `"-"` for local ad-hoc builds. Release builds follow Cliora’s fixed self-signed certificate approach: Tauri builds with `--no-sign`, then `sign-macos-bundles.sh` imports the certificate into a temporary keychain, signs the executable and complete app, replaces the app inside the DMG while preserving its Finder layout, and recreates the updater archive. `verify-macos-bundles.sh` checks all three copies with `codesign --verify --deep --strict` and requires the configured certificate authority. Only then is the updater archive signed and uploaded. A missing certificate, ad-hoc signature or wrong identity fails the release; there is no unsigned fallback. Temporary keychain and certificate files are removed on exit.

The persistent identity is `RillSQL Code Signing`, stored in repository secrets `APPLE_CERTIFICATE` (base64 PKCS#12), `APPLE_CERTIFICATE_PASSWORD`, and `APPLE_SIGNING_IDENTITY`. These were configured with `gh secret set`; preserve the protected backup outside the repository at `~/.config/rillsql/signing/`. Reuse this certificate across releases; do not generate a new key in CI. It is independent of the existing Tauri updater signing key. Do not modify an archive after generating its updater signature.

The signing script registers its temporary keychain in the user search list and as the default, resolves the imported certificate's SHA-1 identity, and confirms that its private key is available before signing. It restores the previous keychain settings on exit. Main CI runs a native signing smoke test on the same macOS runner image as releases, including all three bundle copies, resource-tampering rejection, and keychain restoration. This catches signing environment failures before a release tag is pushed. The v0.2.10 draft stopped at `codesign: no identity found`; its tag is preserved and the fix uses a new patch version.

The fixed self-signed certificate seals the bundle and preserves signing identity across releases, but does not identify an Apple-trusted developer or provide notarization. Browser downloads can still be blocked by Gatekeeper. After moving RillSQL.app to Applications, try opening it and use System Settings → Privacy & Security → Open Anyway when available. If macOS reports “damaged” without that option, users who trust the download from this repository's Release can remove only this app's quarantine attribute:

```sh
xattr -dr com.apple.quarantine "/Applications/RillSQL.app"
open "/Applications/RillSQL.app"
```

Replace the path if installed elsewhere. Do not disable Gatekeeper globally. See [Tauri's signing guide](https://v2.tauri.app/distribute/sign/macos/) for the distinction between ad-hoc signing and Developer ID signing/notarization.

The v0.2.9 DMG was verified against its GitHub asset SHA-256 but failed bundle signature verification: its executable had only a linker-generated signature, with no sealed resources or bound Info.plist. Existing v0.2.9 installations from that trusted release also need a one-time bundle signature repair before opening:

```sh
xattr -dr com.apple.quarantine "/Applications/RillSQL.app"
codesign --force --sign - "/Applications/RillSQL.app"
codesign --verify --deep --strict --verbose=2 "/Applications/RillSQL.app"
open "/Applications/RillSQL.app"
```

This repair is specific to the verified v0.2.9 packaging defect; do not use re-signing to conceal unexplained integrity failures in other downloads. Published assets and tags stay unchanged; the build fix ships in a new version.

## In-app updates

Starting with v0.2.4, Settings → App updates supports manual checks, an optional startup check (enabled by default), download progress, and explicit installation/restart. Startup checks only notify; they never install automatically. Installation refuses active queries, export/import work and unsaved table/structure edits, then checkpoints SQL drafts and the active tab before the installer can exit the application. The same checks run again after download. Older versions without the updater must install v0.2.4 manually once.

Tauri verifies package signatures with the public key in `src-tauri/tauri.conf.json` and requires the signed version to match the announced release. The independent RillSQL signing key and password are configured as `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets; keep their secure backup outside the repository. Preserve this key for future releases so installed clients continue to trust updates. These updater signatures are separate from Windows/macOS platform code signing.

Matrix jobs upload installers, the macOS `.app.tar.gz` update archive, and their `.sig` files. They do not upload competing partial manifests. The single publish job checks all four update packages and signature metadata (key ID and bound version), generates all seven platform/package aliases with immutable tag URLs, and uploads `latest.json` before publishing. Cryptographic package verification is performed by the client updater. Linux DEB and AppImage installations receive their corresponding format; macOS supports ARM64 and Windows supports x64. Stable clients follow the latest stable GitHub release.

Browser verification uses synthetic IPC and cannot prove native installation/relaunch. A full upgrade smoke test requires two signed native versions; do not treat mocked install tests as that evidence.

The macOS matrix must request `--bundles app,dmg`: requesting only `dmg` creates an intermediate `.app` but removes it after packaging. RillSQL retains the app for fixed-certificate signing and, like Cliora, constructs and signs the final updater archive after app signing. The unpublished v0.2.3 draft records the initial missing-archive failure; its tag is preserved.
