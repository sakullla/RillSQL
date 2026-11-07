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
  const cargo = readFileSync(resolve(root, "src-tauri/Cargo.toml"), "utf8")
    .split(/^\[/m).find((section) => section.startsWith("package]"));
  const cargoLock = readFileSync(resolve(root, "src-tauri/Cargo.lock"), "utf8")
    .split(/^\[\[package\]\]/m).find((section) => field(section, "name") === "rillsql");
  return {
    "src-tauri/Cargo.toml": field(cargo, "version"),
    "src-tauri/Cargo.lock rillsql": field(cargoLock, "version"),
  };
}

export function validateVersions(versions, tag) {
  const expected = versions["package.json"] ?? versions["src-tauri/Cargo.toml"];
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

export function checkProject(tag, root = ROOT) {
  const version = validateVersions(readVersions(root), tag);
  const cargo = readFileSync(resolve(root, "src-tauri/Cargo.toml"), "utf8");
  if (!cargo.includes('product-name = "RillSQL"')) {
    throw new Error("Keep product-name as RillSQL: it is used in installation paths and bundle names.");
  }
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
    "Windows 安装包未做平台签名，可能出现 SmartScreen 提示。macOS 使用固定自签证书，未做 Apple Developer ID 签名和公证，首次打开仍可能被系统拦截。",
    "",
    "### macOS 首次打开",
    "",
    "将 RillSQL.app 拖入“应用程序”后打开；如被拦截，可在“系统设置 → 隐私与安全性”中点击“仍要打开”。",
    "如果提示“已损坏”且没有“仍要打开”，仅在确认安装包来自本仓库 Release 且信任该来源后，在终端执行：",
    "",
    "```sh",
    'xattr -dr com.apple.quarantine "/Applications/RillSQL.app"',
    'open "/Applications/RillSQL.app"',
    "```",
    "",
    "以上命令仅移除 RillSQL 的下载隔离标记，不会关闭系统 Gatekeeper。若应用安装在其他目录，请替换路径。", "");
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
