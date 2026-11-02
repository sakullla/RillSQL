import assert from "node:assert/strict";
import { test } from "node:test";
import { checkProject, formatNotes, validateVersions, verifyRelease } from "./release.mjs";
import { waitForMainCi } from "./wait-for-main-ci.cjs";
import { buildUpdaterManifest, updaterFiles, validateSignature } from "./updater.mjs";

const uploaded = (name) => ({ name, state: "uploaded", size: 1024 });
const INSTALLERS = ["RillSQL_0.1.0_x64-setup.exe", "RillSQL_0.1.0_aarch64.dmg", "RillSQL_0.1.0_amd64.deb", "RillSQL_0.1.0_amd64.AppImage"];
const SHA = "a".repeat(40);
const draft = () => ({ tag_name: "v0.1.0", draft: true, prerelease: false, target_commitish: SHA, assets: INSTALLERS.map(uploaded) });

function signedFixture(version = "0.1.0") {
  const packet = Buffer.alloc(74, 1);
  const key = Buffer.alloc(42, 1);
  const pubkey = Buffer.from(`untrusted comment: public key\n${key.toString("base64")}\n`).toString("base64");
  const signature = Buffer.from(`untrusted comment: signature\n${packet.toString("base64")}\ntrusted comment: timestamp:123\tversion:${version}\n${Buffer.alloc(64, 2).toString("base64")}\n`).toString("base64");
  const release = draft();
  for (const name of Object.values(updaterFiles(version))) {
    if (!release.assets.some((asset) => asset.name === name)) release.assets.push(uploaded(name));
    release.assets.push(uploaded(`${name}.sig`));
  }
  return { release, signature, pubkey };
}

test("updater manifest contains every package format and immutable version URLs", () => {
  const { release, signature, pubkey } = signedFixture();
  const manifest = buildUpdaterManifest(release, "v0.1.0", SHA, "sakullla/RillSQL", () => signature, pubkey);
  assert.equal(Object.keys(manifest.platforms).length, 7);
  assert.equal(manifest.platforms["linux-x86_64-deb"].url, "https://github.com/sakullla/RillSQL/releases/download/v0.1.0/RillSQL_0.1.0_amd64.deb");
  assert.equal(manifest.platforms["darwin-aarch64-app"].signature, signature);
  assert.equal(manifest.platforms["windows-x86_64"], manifest.platforms["windows-x86_64-nsis"]);
});

test("updater publication rejects missing or empty signatures and wrong keys or versions", () => {
  const { release, signature, pubkey } = signedFixture();
  release.assets = release.assets.filter((asset) => !asset.name.endsWith(".deb.sig"));
  assert.throws(() => buildUpdaterManifest(release, "v0.1.0", SHA, "sakullla/RillSQL", () => signature, pubkey), /deb.sig/);
  assert.throws(() => validateSignature("", pubkey, "0.1.0"), /encoding/);
  assert.throws(() => validateSignature(signature, pubkey, "0.2.0"), /version/);
  const otherKey = Buffer.from(`untrusted comment: public key\n${Buffer.alloc(42, 3).toString("base64")}\n`).toString("base64");
  assert.throws(() => validateSignature(signature, otherKey, "0.1.0"), /another public key/);
});

test("the checked-out project has consistent release metadata", () => {
  assert.match(checkProject(), /^\d+\.\d+\.\d+/);
});

test("rejects a tag that does not match the project version", () => {
  assert.throws(() => validateVersions({ "package.json": "0.1.0" }, "v0.2.0"), /Release tag must be v0.1.0/);
});

test("rejects version drift between manifests", () => {
  assert.throws(() => validateVersions({ "package.json": "0.1.0", "Cargo.toml": "0.1.1" }), /Cargo.toml/);
});

test("escapes commit subjects and groups them by type", () => {
  const notes = formatNotes(["feat: add *grid*", "fix: <script>", "docs: readme"], "v0.1.0");
  assert.match(notes, /### 新特性\n\n- feat: add \\\*grid\\\*/);
  assert.match(notes, /### 修复\n\n- fix: &lt;script&gt;/);
  assert.match(notes, /### 其他/);
});

test("requires each installer exactly once before publishing", () => {
  const release = draft();
  assert.equal(verifyRelease(release, "v0.1.0").length, 4);
  assert.throws(() => verifyRelease({ ...release, assets: release.assets.slice(1) }, "v0.1.0"), /\.exe/);
  assert.throws(() => verifyRelease({ ...release, draft: false }, "v0.1.0"), /draft/);
});

test("rejects an empty installer", () => {
  const assets = [{ ...uploaded(INSTALLERS[0]), size: 0 }, ...INSTALLERS.slice(1).map(uploaded)];
  assert.throws(() => verifyRelease({ ...draft(), assets }, "v0.1.0"), /empty/);
});

test("rejects mismatched commit, prerelease, version, architecture and duplicate installers", () => {
  assert.equal(verifyRelease(draft(), "v0.1.0", SHA).length, 4);
  assert.throws(() => verifyRelease(draft(), "v0.1.0", "b".repeat(40)), /commit/);
  assert.throws(() => verifyRelease({ ...draft(), prerelease: true }, "v0.1.0", SHA), /prerelease/);
  for (const name of ["RillSQL_0.0.9_x64-setup.exe", "RillSQL_0.1.0_arm64-setup.exe"]) {
    const release = draft();
    release.assets[0].name = name;
    assert.throws(() => verifyRelease(release, "v0.1.0", SHA), /Expected installer/);
  }
  const release = draft();
  release.assets.push(uploaded(INSTALLERS[0]));
  assert.throws(() => verifyRelease(release, "v0.1.0", SHA), /exactly one/);
});

const run = (overrides = {}) => ({ id: 1, head_sha: SHA, head_branch: "main", event: "push", status: "completed", conclusion: "success", html_url: "https://example.test/run", ...overrides });
function gate(runs, comparison = "identical") {
  let calls = 0;
  return {
    sha: SHA, repo: { owner: "test", repo: "rillsql" }, attempts: 2, log() {}, sleep: async () => {},
    github: { rest: {
      actions: { listWorkflowRuns: async (params) => {
        assert.equal(params.head_sha, SHA);
        return { data: { workflow_runs: runs[Math.min(calls++, runs.length - 1)] } };
      } },
      repos: { compareCommitsWithBasehead: async () => ({ data: { status: comparison } }) },
    } },
  };
}

test("main gate waits for the exact commit and latest run", async () => {
  const latest = run({ id: 2 });
  assert.equal(await waitForMainCi(gate([
    [run({ head_sha: "wrong" }), run({ event: "pull_request" }), run(), run({ id: 2, status: "in_progress" })],
    [run(), latest],
  ], "ahead")), latest);
});

test("main gate rejects failed latest runs, missing runs and commits outside main", async () => {
  await assert.rejects(waitForMainCi(gate([[run(), run({ id: 2, conclusion: "failure" })]])), /did not succeed/);
  await assert.rejects(waitForMainCi(gate([[run({ event: "pull_request" }), run({ head_sha: "wrong" })]])), /Timed out/);
  for (const status of ["behind", "diverged"]) {
    await assert.rejects(waitForMainCi(gate([[run()]], status)), /no longer on main/);
  }
});
