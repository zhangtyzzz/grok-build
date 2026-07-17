#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertTargetArtifact } from "./artifact-format.mjs";
import { assertPayloadPolicy, walkFiles } from "./policy.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const targets = JSON.parse(
  fs.readFileSync(path.join(here, "targets.json"), "utf8"),
);
const SHA256_RE = /^[0-9a-f]{64}$/;
const ENVIRONMENT_FINGERPRINT_RE = /^(?:unset|sha256:[0-9a-f]{64})$/;
const TARGET_RUSTFLAGS_NAME_RE =
  /^CARGO_TARGET_[A-Z0-9_]+_RUSTFLAGS$/;
const MACOS_DEPLOYMENT_TARGET_RE = /^[0-9]+(?:\.[0-9]+){1,2}$/;

function fail(message) {
  throw new Error(`distribution verification failed: ${message}`);
}

function sha256File(file) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

function relativePath(root, file) {
  return path.relative(root, file).split(path.sep).join("/");
}

function actualFiles(root, excluded = new Set()) {
  return walkFiles(root)
    .map((file) => ({
      file,
      path: relativePath(root, file),
    }))
    .filter((entry) => !excluded.has(entry.path))
    .sort((a, b) => a.path.localeCompare(b.path, "en"));
}

function treeHash(records, prefix) {
  const hash = crypto.createHash("sha256");
  for (const record of records.filter((item) => item.path.startsWith(prefix))) {
    hash.update(record.path);
    hash.update("\0");
    hash.update(record.sha256);
    hash.update("\n");
  }
  return hash.digest("hex");
}

function validSourceSnapshot(value) {
  return (
    /^[0-9a-f]{40,64}$/i.test(value?.gitCommit ?? "") &&
    /^[0-9a-f]{40,64}$/i.test(value?.sourceRev ?? "") &&
    typeof value?.dirty === "boolean" &&
    SHA256_RE.test(value?.statusSha256 ?? "")
  );
}

function validBuildEnvironment(value) {
  return (
    ENVIRONMENT_FINGERPRINT_RE.test(value?.rustflags ?? "") &&
    ENVIRONMENT_FINGERPRINT_RE.test(value?.cargoEncodedRustflags ?? "") &&
    TARGET_RUSTFLAGS_NAME_RE.test(value?.targetRustflags?.variable ?? "") &&
    ENVIRONMENT_FINGERPRINT_RE.test(
      value?.targetRustflags?.fingerprint ?? "",
    ) &&
    (value?.macosxDeploymentTarget === null ||
      MACOS_DEPLOYMENT_TARGET_RE.test(value?.macosxDeploymentTarget ?? ""))
  );
}

const rootArg = process.argv[2];
if (!rootArg) {
  process.stderr.write("usage: verify.mjs STAGED_ROOT\n");
  process.exit(2);
}
const root = path.resolve(rootArg);
if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) {
  fail(`not a directory: ${root}`);
}

assertPayloadPolicy(root);

const manifestPath = path.join(root, "build-manifest.json");
const checksumPath = path.join(root, "MANIFEST.sha256");
if (!fs.existsSync(manifestPath)) {
  fail("build-manifest.json is missing");
}
if (!fs.existsSync(checksumPath)) {
  fail("MANIFEST.sha256 is missing");
}

let manifest;
try {
  manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
} catch (error) {
  fail(`build-manifest.json is invalid JSON: ${error.message}`);
}
if (manifest.schemaVersion !== 1 || manifest.product !== "grok-build") {
  fail("unsupported manifest schema or product");
}
if (
  typeof manifest.version !== "string" ||
  !/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z._-]+)?$/.test(manifest.version)
) {
  fail("manifest version is invalid");
}
if (
  !manifest.source ||
  !/^[0-9a-f]{40,64}$/i.test(manifest.source.gitCommit ?? "") ||
  !/^[0-9a-f]{40,64}$/i.test(manifest.source.sourceRev ?? "") ||
  typeof manifest.source.dirty !== "boolean" ||
  typeof manifest.source.buildDirty !== "boolean"
) {
  fail("manifest source metadata is invalid");
}
const targetDefinition = targets[manifest.build?.target];
if (!targetDefinition) {
  fail(`manifest target is unsupported: ${manifest.build?.target}`);
}
if (
  manifest.build.profile !== "release-dist" ||
  !Array.isArray(manifest.build.features) ||
  !manifest.build.features.includes("release-dist") ||
  !Number.isSafeInteger(manifest.build.sourceDateEpoch) ||
  typeof manifest.build.rustc !== "string" ||
  manifest.build.rustc.length === 0 ||
  typeof manifest.build.cargo !== "string" ||
  manifest.build.cargo.length === 0
) {
  fail("manifest build metadata is invalid");
}
if (!Array.isArray(manifest.files) || manifest.files.length === 0) {
  fail("manifest contains no payload files");
}

const expectedRecords = [...manifest.files].sort((a, b) =>
  a.path.localeCompare(b.path, "en"),
);
const seenManifestPaths = new Set();
for (const record of expectedRecords) {
  const components =
    typeof record.path === "string" ? record.path.split("/") : [];
  if (
    components.length === 0 ||
    components.some((component) => component.length === 0) ||
    path.posix.isAbsolute(record.path) ||
    components.includes(".") ||
    components.includes("..") ||
    seenManifestPaths.has(record.path) ||
    !Number.isSafeInteger(record.size) ||
    record.size < 0 ||
    !/^[0-9a-f]{64}$/.test(record.sha256 ?? "") ||
    typeof record.executable !== "boolean"
  ) {
    fail(`invalid manifest file record: ${JSON.stringify(record)}`);
  }
  seenManifestPaths.add(record.path);
}
const actualPayload = actualFiles(
  root,
  new Set(["build-manifest.json", "MANIFEST.sha256"]),
);
if (expectedRecords.length !== actualPayload.length) {
  fail(
    `payload file count differs: manifest=${expectedRecords.length} actual=${actualPayload.length}`,
  );
}

for (let i = 0; i < expectedRecords.length; i += 1) {
  const expected = expectedRecords[i];
  const actual = actualPayload[i];
  if (expected.path !== actual.path) {
    fail(`payload path differs: expected ${expected.path}, found ${actual.path}`);
  }
  const stat = fs.statSync(actual.file);
  const digest = sha256File(actual.file);
  if (stat.size !== expected.size) {
    fail(`size mismatch for ${actual.path}`);
  }
  if (digest !== expected.sha256) {
    fail(`sha256 mismatch for ${actual.path}`);
  }
  if (process.platform !== "win32") {
    const executable = (stat.mode & 0o111) !== 0;
    if (executable !== expected.executable) {
      fail(`executable bit mismatch for ${actual.path}`);
    }
  }
}

const artifact = expectedRecords.find(
  (record) => record.path === manifest.artifact?.path,
);
if (
  !artifact ||
  manifest.artifact.path !== `bin/${targetDefinition.executable}` ||
  artifact.size !== manifest.artifact.size ||
  artifact.sha256 !== manifest.artifact.sha256
) {
  fail("artifact metadata does not match the payload");
}

let attestation = null;
let hasValidAttestation = false;
if (manifest.buildAttestation !== null) {
  const attestationRecord = expectedRecords.find(
    (record) => record.path === "build-attestation.json",
  );
  if (
    !attestationRecord ||
    manifest.buildAttestation?.path !== attestationRecord.path ||
    manifest.buildAttestation?.sha256 !== attestationRecord.sha256 ||
    manifest.buildAttestation?.schemaVersion !== 1
  ) {
    fail("build attestation metadata does not match the payload");
  }
  try {
    attestation = JSON.parse(
      fs.readFileSync(path.join(root, "build-attestation.json"), "utf8"),
    );
  } catch (error) {
    fail(`build-attestation.json is invalid JSON: ${error.message}`);
  }
  if (
    attestation.schemaVersion !== 1 ||
    attestation.product !== "grok-build" ||
    attestation.version !== manifest.version ||
    attestation.target !== manifest.build.target ||
    attestation.source?.gitCommit !== manifest.source.gitCommit ||
    attestation.source?.sourceRev !== manifest.source.sourceRev ||
    attestation.source?.dirty !== manifest.source.buildDirty ||
    !validSourceSnapshot(attestation.source?.buildStart) ||
    !validSourceSnapshot(attestation.source?.buildEnd) ||
    attestation.source?.buildStart?.gitCommit !==
      attestation.source?.gitCommit ||
    attestation.source?.buildStart?.sourceRev !==
      attestation.source?.sourceRev ||
    attestation.source?.buildEnd?.gitCommit !== attestation.source?.gitCommit ||
    attestation.source?.buildEnd?.sourceRev !== attestation.source?.sourceRev ||
    attestation.source?.dirty !==
      (attestation.source?.buildStart?.dirty ||
        attestation.source?.buildEnd?.dirty) ||
    attestation.build?.profile !== manifest.build.profile ||
    JSON.stringify(attestation.build?.features) !==
      JSON.stringify(manifest.build.features) ||
    attestation.build?.rustc !== manifest.build.rustc ||
    attestation.build?.cargo !== manifest.build.cargo ||
    !validBuildEnvironment(attestation.build?.environment) ||
    attestation.artifact?.size !== artifact.size ||
    attestation.artifact?.sha256 !== artifact.sha256
  ) {
    fail("build attestation does not match manifest source/build/artifact");
  }
  assertTargetArtifact(
    manifest.build.target,
    path.join(root, manifest.artifact.path),
  );
  hasValidAttestation = true;
} else if (
  expectedRecords.some((record) => record.path === "build-attestation.json")
) {
  fail("payload contains an undeclared build attestation");
}

const profileRecords = expectedRecords.filter((record) =>
  record.path.startsWith("profiles/starter/"),
);
if (
  profileRecords.length !== manifest.profilePack?.fileCount ||
  treeHash(expectedRecords, "profiles/starter/") !==
    manifest.profilePack?.sha256
) {
  fail("starter profile metadata does not match the payload");
}
if (
  manifest.profilePack.path !== "profiles/starter" ||
  manifest.licenses?.project !== "LICENSE" ||
  manifest.licenses?.thirdParty !== "THIRD-PARTY-NOTICES" ||
  manifest.licenses?.bundledTools !== "BUNDLED-TOOLS-NOTICES.md"
) {
  fail("profile or license metadata is invalid");
}
const requiredPayloadPaths = [
  "LICENSE",
  "THIRD-PARTY-NOTICES",
  "BUNDLED-TOOLS-NOTICES.md",
  "SOURCE_REV",
  "profiles/starter/README.md",
  "profiles/starter/config.toml",
];
for (const requiredPath of requiredPayloadPaths) {
  if (!expectedRecords.some((record) => record.path === requiredPath)) {
    fail(`required payload file is missing: ${requiredPath}`);
  }
}
const packagedSourceRev = fs
  .readFileSync(path.join(root, "SOURCE_REV"), "utf8")
  .trim();
if (packagedSourceRev !== manifest.source.sourceRev) {
  fail("SOURCE_REV does not match manifest source metadata");
}

if (!Array.isArray(manifest.bundledTools)) {
  fail("bundledTools must be an array");
}
const bundledToolNames = new Set();
for (const tool of manifest.bundledTools) {
  if (
    typeof tool.name !== "string" ||
    tool.name.length === 0 ||
    bundledToolNames.has(tool.name) ||
    typeof tool.version !== "string" ||
    tool.version.length === 0 ||
    !Number.isSafeInteger(tool.size) ||
    tool.size < 0 ||
    !/^[0-9a-f]{64}$/.test(tool.sha256 ?? "")
  ) {
    fail(`invalid bundled tool record: ${JSON.stringify(tool)}`);
  }
  bundledToolNames.add(tool.name);
}
if (
  hasValidAttestation &&
  JSON.stringify(attestation.bundledTools) !==
    JSON.stringify(manifest.bundledTools)
) {
  fail("attested bundled-tool inputs do not match the manifest");
}
const requiredTools =
  targetDefinition.platform === "windows"
    ? ["ripgrep"]
    : ["bfs", "ripgrep", "ugrep"];
const computedReleaseReady =
  !manifest.source.dirty &&
  !manifest.source.buildDirty &&
  hasValidAttestation &&
  requiredTools.every((name) => bundledToolNames.has(name));
if (manifest.releaseReady !== computedReleaseReady) {
  fail("releaseReady does not match source cleanliness and bundled tools");
}

const checksumLines = fs
  .readFileSync(checksumPath, "utf8")
  .split(/\r?\n/)
  .filter(Boolean);
const checksumRecords = new Map();
for (const line of checksumLines) {
  const match = line.match(/^([0-9a-f]{64})  ([^\0\r\n]+)$/);
  if (!match) {
    fail(`invalid MANIFEST.sha256 line: ${JSON.stringify(line)}`);
  }
  const relative = match[2];
  const components = relative.split("/");
  if (
    path.posix.isAbsolute(relative) ||
    components.includes("..") ||
    components.includes(".")
  ) {
    fail(`unsafe checksum path: ${relative}`);
  }
  if (checksumRecords.has(relative)) {
    fail(`duplicate checksum path: ${relative}`);
  }
  checksumRecords.set(relative, match[1]);
}

const checksumPayload = actualFiles(root, new Set(["MANIFEST.sha256"]));
if (checksumRecords.size !== checksumPayload.length) {
  fail(
    `internal checksum file count differs: checksums=${checksumRecords.size} actual=${checksumPayload.length}`,
  );
}
for (const entry of checksumPayload) {
  const expected = checksumRecords.get(entry.path);
  if (!expected) {
    fail(`missing internal checksum for ${entry.path}`);
  }
  if (sha256File(entry.file) !== expected) {
    fail(`internal checksum mismatch for ${entry.path}`);
  }
}

process.stdout.write(
  `verified grok-build ${manifest.version} for ${manifest.build.target}: ${expectedRecords.length} payload files\n`,
);
