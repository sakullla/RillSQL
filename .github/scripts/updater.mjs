import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyRelease } from "./release.mjs";

export function updaterFiles(version) {
  return {
    "windows-x86_64": `RillSQL_${version}_x64-setup.exe`,
    "linux-x86_64": `RillSQL_${version}_amd64.AppImage`,
    "linux-x86_64-deb": `RillSQL_${version}_amd64.deb`,
    "darwin-aarch64": `RillSQL_${version}_aarch64.app.tar.gz`,
  };
}

function decode(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9+/]+={0,2}$/.test(value.trim())) throw new Error("Invalid updater signature encoding.");
  return Buffer.from(value.trim(), "base64");
}

export function validateSignature(signature, publicKey, version) {
  const lines = decode(signature).toString("utf8").trim().split(/\r?\n/);
  const publicLines = decode(publicKey).toString("utf8").trim().split(/\r?\n/);
  const packet = decode(lines[1]);
  const key = decode(publicLines[1]);
  if (lines.length !== 4 || packet.length !== 74 || key.length !== 42 || decode(lines[3]).length !== 64) {
    throw new Error("Incomplete updater signature.");
  }
  if (!packet.subarray(2, 10).equals(key.subarray(2, 10))) throw new Error("Updater signature belongs to another public key.");
  if (!lines[2].startsWith("trusted comment: ") || !lines[2].split("\t").includes(`version:${version}`)) {
    throw new Error("Updater signature is not bound to this release version.");
  }
  return signature.trim();
}

export function buildUpdaterManifest(release, tag, sha, repo, readSignature, publicKey, now = new Date()) {
  verifyRelease(release, tag, sha);
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo)) throw new Error("Invalid GitHub repository.");
  const version = tag.slice(1);
  const platforms = {};
  for (const [target, name] of Object.entries(updaterFiles(version))) {
    for (const required of [name, `${name}.sig`]) {
      const matches = release.assets.filter((asset) => asset.name === required);
      if (matches.length !== 1 || matches[0].state !== "uploaded" || !(matches[0].size > 0)) {
        throw new Error(`Missing, duplicate or incomplete updater artifact: ${required}`);
      }
    }
    platforms[target] = {
      signature: validateSignature(readSignature(`${name}.sig`), publicKey, version),
      url: `https://github.com/${repo}/releases/download/${tag}/${name}`,
    };
  }
  platforms["windows-x86_64-nsis"] = platforms["windows-x86_64"];
  platforms["linux-x86_64-appimage"] = platforms["linux-x86_64"];
  platforms["darwin-aarch64-app"] = platforms["darwin-aarch64"];
  return { version, notes: release.body ?? "", pub_date: now.toISOString(), platforms };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [releasePath, tag, sha, directory, repo] = process.argv.slice(2);
    if (!sha || !/^[a-f0-9]{40}$/.test(sha) || !directory) throw new Error("Usage: updater.mjs <release.json> <tag> <sha> <signatures-dir> <owner/repo>");
    const config = JSON.parse(readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
    const manifest = buildUpdaterManifest(JSON.parse(readFileSync(releasePath, "utf8")), tag, sha, repo,
      (name) => readFileSync(resolve(directory, name), "utf8"), config.plugins.updater.pubkey);
    process.stdout.write(`${JSON.stringify(manifest, null, 2)}\n`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
