import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "node:child_process";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?$/;
const INSTALLERS = [".exe", ".dmg", ".deb", ".AppImage"];

function json(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function field(section, name) {
  return section?.match(new RegExp(`^${name}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
}

export function readVersions(root = ROOT) {
  const pkg = json(resolve(root, "package.json"));
  const lock = json(resolve(root, "package-lock.json"));
  const cargo = readFileSync(resolve(root, "src-tauri/Cargo.toml"), "utf8")
    .split(/^\[/m).find((section) => section.startsWith("package]"));
  const cargoLock = readFileSync(resolve(root, "src-tauri/Cargo.lock"), "utf8")
    .split(/^\[\[package\]\]/m).find((section) => field(section, "name") === "rillsql");
  return {
    "package.json": pkg.version,
    "package-lock.json": lock.version,
    "package-lock.json packages[root]": lock.packages?.[""]?.version,
    "src-tauri/Cargo.toml": field(cargo, "version"),
    "src-tauri/Cargo.lock rillsql": field(cargoLock, "version"),
    "src-tauri/tauri.conf.json": json(resolve(root, "src-tauri/tauri.conf.json")).version,
    "src/shell/product.ts": readFileSync(resolve(root, "src/shell/product.ts"), "utf8").match(/PRODUCT_VERSION = "([^"]+)"/)?.[1],
  };
}

export function validateVersions(versions, tag) {
  const expected = versions["package.json"];
  if (typeof expected !== "string" || !SEMVER.test(expected)) {
    throw new Error("Use a SemVer version such as 0.1.0 or 0.1.0-rc.1 (without build metadata).");
  }
  for (const [path, version] of Object.entries(versions)) {
    if (version !== expected) throw new Error(`${path}: expected ${expected}, got ${version}`);
  }
  if (tag !== undefined && tag !== `v${expected}`) {
    throw new Error(`Release tag must be v${expected}, got ${tag}`);
  }
  return expected;
}

export function checkTauriVersions(root = ROOT) {
  const lock = json(resolve(root, "package-lock.json"));
  const api = lock.packages?.["node_modules/@tauri-apps/api"]?.version;
  const crates = readFileSync(resolve(root, "src-tauri/Cargo.lock"), "utf8")
    .split(/^\[\[package\]\]/m).filter((section) => field(section, "name") === "tauri");
  const tauri = crates.length === 1 ? field(crates[0], "version") : undefined;
  if (!api || !tauri || !SEMVER.test(api) || !SEMVER.test(tauri)) {
    throw new Error("Lockfiles must contain one tauri crate and @tauri-apps/api with valid versions.");
  }
  // Tauri's build command requires the Rust crate and JS API to share major/minor.
  if (tauri.split(".").slice(0, 2).join(".") !== api.split(".").slice(0, 2).join(".")) {
    throw new Error(`Tauri version mismatch: tauri ${tauri} (Cargo.lock) vs @tauri-apps/api ${api} (package-lock.json).`);
  }
}

export function checkProject(tag, root = ROOT) {
  const version = validateVersions(readVersions(root), tag);
  checkTauriVersions(root);
  return version;
}

export function formatNotes(subjects, previous) {
  const groups = { "新特性": [], "修复": [], "其他": [] };
  for (const subject of subjects) {
    const group = /^(feat|perf)(\([^)]*\))?!?:/.test(subject) ? "新特性"
      : /^fix(\([^)]*\))?!?:/.test(subject) ? "修复" : "其他";
    // Render commit subjects as plain text, without HTML or Markdown injection.
    const safe = subject.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/([\\`*_{}\[\]()#+.!|~])/g, "\\$1");
    groups[group].push(`- ${safe}`);
  }
  const lines = [`本版本变更（${previous ? `自 ${previous}` : "首个发布"}）：`, ""];
  for (const [heading, entries] of Object.entries(groups)) {
    if (entries.length) lines.push(`### ${heading}`, "", ...entries, "");
  }
  lines.push("---", "",
    "Windows x64：NSIS EXE；macOS Apple Silicon：DMG；Linux x64：DEB / AppImage。",
    "",
    "安装包未做平台签名：Windows 可能出现 SmartScreen 提示；macOS 首次打开需在“隐私与安全性”中允许。", "");
  return lines.join("\n");
}

export function generateNotes(tag, root = ROOT) {
  checkProject(tag, root);
  // Only an ancestor of this release is eligible; unrelated newer tags are excluded.
  const previousResult = spawnSync("git", ["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*", `${tag}^`],
    { cwd: root, encoding: "utf8" });
  const previous = previousResult.status === 0 ? previousResult.stdout.trim() : "";
  const range = previous ? `${previous}..${tag}` : tag;
  const subjects = execFileSync("git", ["log", range, "--format=%s", "--no-merges"],
    { cwd: root, encoding: "utf8" }).trim().split("\n").filter(Boolean);
  return formatNotes(subjects, previous);
}

export function verifyRelease(release, tag, sha) {
  if (!tag || release.tag_name !== tag) throw new Error("Release tag does not match this run.");
  const version = tag.slice(1);
  if (!tag.startsWith("v") || !SEMVER.test(version)) throw new Error("Invalid release version.");
  if (release.draft !== true) throw new Error("Release must remain a draft until verification succeeds.");
  if (sha && release.target_commitish !== sha) throw new Error("Release commit does not match the verified SHA.");
  if (release.prerelease !== version.includes("-")) throw new Error("Release prerelease flag does not match the version.");
  const assets = release.assets ?? [];
  const expected = [`RillSQL_${version}_x64-setup.exe`, `RillSQL_${version}_aarch64.dmg`,
    `RillSQL_${version}_amd64.deb`, `RillSQL_${version}_amd64.AppImage`];
  for (const [index, suffix] of INSTALLERS.entries()) {
    const matches = assets.filter((asset) => asset.name.endsWith(suffix));
    if (matches.length !== 1) throw new Error(`Expected exactly one ${suffix} installer, found ${matches.length}.`);
    if (matches[0].name !== expected[index]) throw new Error(`Expected installer ${expected[index]}, got ${matches[0].name}.`);
    if (matches[0].state !== "uploaded" || !(matches[0].size > 0)) {
      throw new Error(`Installer is empty or upload is incomplete: ${matches[0].name}`);
    }
  }
  return assets.map((asset) => asset.name);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [command, argument, tag, sha] = process.argv.slice(2);
    if (command === "check") console.log(`Release metadata verified: ${checkProject(argument)}`);
    else if (command === "notes") process.stdout.write(generateNotes(argument));
    else if (command === "verify") {
      if (!sha || !/^[a-f0-9]{40}$/.test(sha)) throw new Error("verify requires the verified commit SHA.");
      console.log(verifyRelease(json(argument), tag, sha).join("\n"));
    } else throw new Error("Usage: release.mjs check [tag] | notes <tag> | verify <release.json> <tag> <sha>");
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
