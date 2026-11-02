# CI and releases

CI builds the frontend once, then shares `frontend-dist` with the production-browser checks and the three native Rust test/Clippy jobs. Browser checks use synthetic IPC; they do not replace native database verification.

On a main push, three cache jobs prepare release-profile Rust dependencies when their cache is missing. Their `release-v1-${target}` shared keys and workspace configuration match the release workflow. Tags read main's caches and do not save tag-scoped copies. The first main run after a toolchain or dependency change can take longer; subsequent releases reuse those compiled dependencies. Application crates are rebuilt for each release.

A tag release waits for the latest `push` main CI run for its exact commit, and checks that the commit is still on main. It does not rerun the full CI suite. All build jobs check out that immutable SHA, and an existing draft can only be resumed if its target SHA matches. Before publication, the workflow checks the draft flag, prerelease flag, target SHA, and exactly one nonempty uploaded installer with each expected version and architecture filename. It then resolves the remote tag again to reject a moved tag.

Use the sequence in `AGENTS.md`: validate, push the release branch and main, wait for main CI, then push an annotated version tag. Rerun transient infrastructure failures on the same commit. For code fixes, increment the patch version; never move a pushed release tag. Published releases cannot be overwritten by this workflow.

Run the release guard tests locally with `node --test .github/scripts/release.test.mjs`. They cover manifest consistency, asset validation, and the main CI gate (including wrong commits, PR runs, failed newer runs and commits outside main).
