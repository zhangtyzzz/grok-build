import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const internalVersion = "0.0.0";
export const libraries = ["xai-grok-pager", "xai-grok-shell", "xai-grok-version"];
const binary = "xai-grok-pager-bin";
const releasePattern = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(alpha|beta|rc)\.(0|[1-9][0-9]*))?$/;

function field(section, name) {
  const matches = [...section.matchAll(new RegExp(`^${name} = "([^"\\r\\n]+)"\\r?$`, "gm"))];
  if (matches.length !== 1) throw new Error(`expected one ${name} field`);
  return matches[0][1];
}

export function releaseVersion(root, expected) {
  const versions = new Map();
  for (const name of [binary, ...libraries]) {
    const source = fs.readFileSync(path.join(root, "crates/codegen", name, "Cargo.toml"), "utf8");
    const parts = source.split(/^\[package\]\r?$/m);
    if (parts.length !== 2) throw new Error(`missing or duplicate package section: ${name}`);
    const section = parts[1].split(/^\[/m)[0];
    if (field(section, "name") !== name) throw new Error(`unexpected package name: ${name}`);
    versions.set(name, field(section, "version"));
  }
  const version = versions.get(binary);
  if (!releasePattern.test(version)) throw new Error(`unsupported release version: ${version}`);
  if (expected !== undefined && expected !== version) {
    throw new Error(`release version ${expected} does not match binary version ${version}`);
  }
  for (const name of libraries) {
    if (versions.get(name) !== internalVersion) throw new Error(`${name} must retain internal version ${internalVersion}`);
  }
  const lock = fs.readFileSync(path.join(root, "Cargo.lock"), "utf8");
  const packages = lock.split(/^\[\[package\]\]\r?$/m).slice(1);
  for (const [name, version] of versions) {
    const matches = packages.filter(section => field(section, "name") === name);
    if (matches.length !== 1 || field(matches[0], "version") !== version) {
      throw new Error(`Cargo.lock does not match ${name} version ${version}`);
    }
  }
  return version;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const root = fileURLToPath(new URL("../../", import.meta.url));
    console.log(releaseVersion(root, process.argv[2]));
  } catch (error) {
    console.error(`release-version: ${error.message}`);
    process.exitCode = 1;
  }
}
