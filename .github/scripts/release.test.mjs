import assert from "node:assert/strict";
import { test } from "node:test";
import { checkProject, formatNotes, validateVersions, verifyRelease } from "./release.mjs";

const uploaded = (name) => ({ name, state: "uploaded", size: 1024 });
const INSTALLERS = ["a_0.1.0_x64-setup.exe", "a_0.1.0_aarch64.dmg", "a_0.1.0_amd64.deb", "a_0.1.0_amd64.AppImage"];

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
  const release = { tag_name: "v0.1.0", draft: true, assets: INSTALLERS.map(uploaded) };
  assert.equal(verifyRelease(release, "v0.1.0").length, 4);
  assert.throws(() => verifyRelease({ ...release, assets: release.assets.slice(1) }, "v0.1.0"), /\.exe/);
  assert.throws(() => verifyRelease({ ...release, draft: false }, "v0.1.0"), /draft/);
});

test("rejects an empty installer", () => {
  const assets = [{ ...uploaded(INSTALLERS[0]), size: 0 }, ...INSTALLERS.slice(1).map(uploaded)];
  assert.throws(() => verifyRelease({ tag_name: "v0.1.0", draft: true, assets }, "v0.1.0"), /empty/);
});
