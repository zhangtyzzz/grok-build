#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z._-]+)?$/;
const REVISION_RE = /^[0-9a-f]{40,64}$/i;
const LABEL_RE = /^[0-9A-Za-z][0-9A-Za-z._+-]*$/;
const TARGET_RE = /^[0-9A-Za-z][0-9A-Za-z._-]*$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const ENVIRONMENT_FINGERPRINT_RE = /^(?:unset|sha256:[0-9a-f]{64})$/;
const TARGET_RUSTFLAGS_NAME_RE =
  /^CARGO_TARGET_[A-Z0-9_]+_RUSTFLAGS$/;
const MACOS_DEPLOYMENT_TARGET_RE = /^[0-9]+(?:\.[0-9]+){1,2}$/;

function fail(message) {
  throw new Error(`build attestation: ${message}`);
}

function parseArgs(argv) {
  const values = { bundledTools: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i];
    if (!key.startsWith("--")) {
      fail(`unexpected argument: ${key}`);
    }
    const value = argv[++i];
    if (value === undefined) {
      fail(`missing value for ${key}`);
    }
    const name = key.slice(2);
    if (name === "bundled-tool") {
      values.bundledTools.push(value);
    } else {
      values[name] = value;
    }
  }
  return values;
}

function required(args, name) {
  const value = args[name];
  if (typeof value !== "string" || value.length === 0) {
    fail(`--${name} is required`);
  }
  return value;
}

function requiredBoundedText(args, name) {
  const value = required(args, name);
  if (value.length > 512 || /[\0\r\n]/.test(value)) {
    fail(`--${name} must be at most 512 bytes and contain no line breaks`);
  }
  return value;
}

function requiredBoolean(args, name) {
  const value = required(args, name);
  if (value !== "true" && value !== "false") {
    fail(`--${name} must be true or false`);
  }
  return value === "true";
}

function validateRevision(value, label) {
  if (!REVISION_RE.test(value)) {
    fail(`${label} must be a 40-64 character hexadecimal revision`);
  }
  return value;
}

function parseFeatures(value) {
  const features = value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean)
    .sort();
  if (features.length === 0 || features.some((item) => !LABEL_RE.test(item))) {
    fail("features must be a non-empty comma-separated list of safe labels");
  }
  if (new Set(features).size !== features.length) {
    fail("features must not contain duplicates");
  }
  return features;
}

function parseSourceSnapshot(args, prefix) {
  return {
    gitCommit: validateRevision(
      required(args, `${prefix}-git-commit`),
      `source.${prefix}.gitCommit`,
    ),
    sourceRev: validateRevision(
      required(args, `${prefix}-source-rev`),
      `source.${prefix}.sourceRev`,
    ),
    dirty: requiredBoolean(args, `${prefix}-dirty`),
    statusSha256: required(args, `${prefix}-status-sha256`),
  };
}

function validateSourceSnapshot(value, label) {
  validateRevision(value?.gitCommit ?? "", `${label}.gitCommit`);
  validateRevision(value?.sourceRev ?? "", `${label}.sourceRev`);
  if (
    typeof value?.dirty !== "boolean" ||
    !SHA256_RE.test(value?.statusSha256 ?? "")
  ) {
    fail(`${label} dirty/status fingerprint is invalid`);
  }
}

function requiredEnvironmentFingerprint(args, name) {
  const value = required(args, name);
  if (!ENVIRONMENT_FINGERPRINT_RE.test(value)) {
    fail(`--${name} must be unset or a sha256 fingerprint`);
  }
  return value;
}

function parseBuildEnvironment(args) {
  const targetVariable = required(args, "target-rustflags-name");
  if (!TARGET_RUSTFLAGS_NAME_RE.test(targetVariable)) {
    fail("--target-rustflags-name is invalid");
  }
  const macosValue = required(args, "macosx-deployment-target");
  if (
    macosValue !== "unset" &&
    !MACOS_DEPLOYMENT_TARGET_RE.test(macosValue)
  ) {
    fail("--macosx-deployment-target is invalid");
  }
  return {
    rustflags: requiredEnvironmentFingerprint(args, "rustflags"),
    cargoEncodedRustflags: requiredEnvironmentFingerprint(
      args,
      "cargo-encoded-rustflags",
    ),
    targetRustflags: {
      variable: targetVariable,
      fingerprint: requiredEnvironmentFingerprint(args, "target-rustflags"),
    },
    macosxDeploymentTarget: macosValue === "unset" ? null : macosValue,
  };
}

function validateBuildEnvironment(value) {
  if (
    !ENVIRONMENT_FINGERPRINT_RE.test(value?.rustflags ?? "") ||
    !ENVIRONMENT_FINGERPRINT_RE.test(value?.cargoEncodedRustflags ?? "") ||
    !TARGET_RUSTFLAGS_NAME_RE.test(value?.targetRustflags?.variable ?? "") ||
    !ENVIRONMENT_FINGERPRINT_RE.test(
      value?.targetRustflags?.fingerprint ?? "",
    ) ||
    (value?.macosxDeploymentTarget !== null &&
      !MACOS_DEPLOYMENT_TARGET_RE.test(value?.macosxDeploymentTarget ?? ""))
  ) {
    fail("build environment metadata is invalid");
  }
}

function sha256File(file) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

function fileRecord(file) {
  const stat = fs.statSync(file);
  if (!stat.isFile()) {
    fail(`not a regular file: ${file}`);
  }
  return {
    size: stat.size,
    sha256: sha256File(file),
  };
}

function parseBundledTool(spec) {
  const first = spec.indexOf(",");
  const second = spec.indexOf(",", first + 1);
  if (first <= 0 || second <= first + 1 || second === spec.length - 1) {
    fail(`invalid --bundled-tool ${JSON.stringify(spec)}`);
  }
  const name = spec.slice(0, first);
  const version = spec.slice(first + 1, second);
  if (!LABEL_RE.test(name) || !LABEL_RE.test(version)) {
    fail(`bundled-tool name/version contains an unsafe label: ${spec}`);
  }
  const file = path.resolve(spec.slice(second + 1));
  return { name, version, ...fileRecord(file) };
}

function validateAttestation(value) {
  if (!VERSION_RE.test(value.version ?? "")) {
    fail("version is invalid");
  }
  if (!TARGET_RE.test(value.target ?? "")) {
    fail("target is invalid");
  }
  validateRevision(value.source?.gitCommit ?? "", "source.gitCommit");
  validateRevision(value.source?.sourceRev ?? "", "source.sourceRev");
  if (typeof value.source?.dirty !== "boolean") {
    fail("source.dirty must be boolean");
  }
  validateSourceSnapshot(value.source?.buildStart, "source.buildStart");
  validateSourceSnapshot(value.source?.buildEnd, "source.buildEnd");
  if (
    value.source.gitCommit !== value.source.buildEnd.gitCommit ||
    value.source.sourceRev !== value.source.buildEnd.sourceRev ||
    value.source.gitCommit !== value.source.buildStart.gitCommit ||
    value.source.sourceRev !== value.source.buildStart.sourceRev ||
    value.source.dirty !==
      (value.source.buildStart.dirty || value.source.buildEnd.dirty)
  ) {
    fail("source identity changed during the build or dirty state is invalid");
  }
  if (
    typeof value.build?.profile !== "string" ||
    value.build.profile.length === 0 ||
    !Array.isArray(value.build?.features) ||
    value.build.features.length === 0 ||
    JSON.stringify(value.build.features) !==
      JSON.stringify([...value.build.features].sort()) ||
    new Set(value.build.features).size !== value.build.features.length ||
    value.build.features.some(
      (item) => typeof item !== "string" || !LABEL_RE.test(item),
    ) ||
    typeof value.build?.rustc !== "string" ||
    value.build.rustc.length === 0 ||
    /[\0\r\n]/.test(value.build.rustc) ||
    typeof value.build?.cargo !== "string" ||
    value.build.cargo.length === 0 ||
    /[\0\r\n]/.test(value.build.cargo) ||
    !Number.isSafeInteger(value.build?.sourceDateEpoch) ||
    value.build.sourceDateEpoch < 0
  ) {
    fail("build metadata is invalid");
  }
  validateBuildEnvironment(value.build.environment);
  if (
    typeof value.artifact?.fileName !== "string" ||
    value.artifact.fileName.length === 0 ||
    path.basename(value.artifact.fileName) !== value.artifact.fileName ||
    !Number.isSafeInteger(value.artifact?.size) ||
    value.artifact.size < 0 ||
    !SHA256_RE.test(value.artifact?.sha256 ?? "")
  ) {
    fail("artifact metadata is invalid");
  }
  if (!Array.isArray(value.bundledTools)) {
    fail("bundledTools must be an array");
  }
  const names = new Set();
  let previous = "";
  for (const tool of value.bundledTools) {
    if (
      typeof tool?.name !== "string" ||
      !LABEL_RE.test(tool.name) ||
      names.has(tool.name) ||
      (previous && previous.localeCompare(tool.name, "en") >= 0) ||
      typeof tool.version !== "string" ||
      !LABEL_RE.test(tool.version) ||
      !Number.isSafeInteger(tool.size) ||
      tool.size < 0 ||
      !SHA256_RE.test(tool.sha256 ?? "")
    ) {
      fail(`invalid bundled-tool metadata: ${JSON.stringify(tool)}`);
    }
    names.add(tool.name);
    previous = tool.name;
  }
}

function parseAttestation(file) {
  let value;
  try {
    value = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    fail(`cannot parse ${file}: ${error.message}`);
  }
  if (value.schemaVersion !== 1 || value.product !== "grok-build") {
    fail("unsupported schema or product");
  }
  validateAttestation(value);
  return value;
}

function sortedTools(specs) {
  const tools = specs
    .map(parseBundledTool)
    .sort((a, b) => a.name.localeCompare(b.name, "en"));
  const names = new Set();
  for (const tool of tools) {
    if (names.has(tool.name)) {
      fail(`duplicate bundled tool: ${tool.name}`);
    }
    names.add(tool.name);
  }
  return tools;
}

function create(args) {
  const output = path.resolve(required(args, "output"));
  const binary = path.resolve(required(args, "binary"));
  const sourceDateEpoch = Number(required(args, "source-date-epoch"));
  if (!Number.isSafeInteger(sourceDateEpoch) || sourceDateEpoch < 0) {
    fail("source date epoch must be a non-negative integer");
  }
  const artifact = fileRecord(binary);
  const version = required(args, "version");
  if (!VERSION_RE.test(version)) {
    fail("version is invalid");
  }
  const target = required(args, "target");
  if (!TARGET_RE.test(target)) {
    fail("target is invalid");
  }
  const buildStart = parseSourceSnapshot(args, "build-start");
  const buildEnd = parseSourceSnapshot(args, "build-end");
  const dirty = requiredBoolean(args, "dirty");
  const attestation = {
    schemaVersion: 1,
    product: "grok-build",
    version,
    target,
    source: {
      gitCommit: validateRevision(
        required(args, "git-commit"),
        "source.gitCommit",
      ),
      sourceRev: validateRevision(
        required(args, "source-rev"),
        "source.sourceRev",
      ),
      dirty,
      buildStart,
      buildEnd,
    },
    build: {
      profile: required(args, "profile"),
      features: parseFeatures(required(args, "features")),
      rustc: requiredBoundedText(args, "rustc"),
      cargo: requiredBoundedText(args, "cargo"),
      sourceDateEpoch,
      environment: parseBuildEnvironment(args),
    },
    artifact: {
      fileName: path.basename(binary),
      ...artifact,
    },
    bundledTools: sortedTools(args.bundledTools),
  };
  validateAttestation(attestation);
  fs.writeFileSync(output, `${JSON.stringify(attestation, null, 2)}\n`, "utf8");
}

function verify(args) {
  const attestationPath = path.resolve(required(args, "attestation"));
  const binary = path.resolve(required(args, "binary"));
  const value = parseAttestation(attestationPath);
  const expected = {
    version: required(args, "version"),
    target: required(args, "target"),
    gitCommit: required(args, "git-commit"),
    sourceRev: required(args, "source-rev"),
  };
  if (value.version !== expected.version || value.target !== expected.target) {
    fail(
      `version/target mismatch: attested=${value.version}/${value.target} expected=${expected.version}/${expected.target}`,
    );
  }
  if (
    value.source?.gitCommit !== expected.gitCommit ||
    value.source?.sourceRev !== expected.sourceRev
  ) {
    fail("source revision does not match the packaging checkout");
  }
  if (
    value.build?.profile !== "release-dist" ||
    !Array.isArray(value.build?.features) ||
    !value.build.features.includes("release-dist")
  ) {
    fail("artifact was not built with the release-dist profile and feature");
  }
  const actualArtifact = fileRecord(binary);
  if (
    value.artifact?.fileName !== path.basename(binary) ||
    value.artifact?.size !== actualArtifact.size ||
    value.artifact?.sha256 !== actualArtifact.sha256
  ) {
    fail("binary hash/size/name does not match the attestation");
  }
  const expectedTools = sortedTools(args.bundledTools);
  if (JSON.stringify(value.bundledTools) !== JSON.stringify(expectedTools)) {
    fail("bundled-tool inputs do not match the build attestation");
  }
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function field(file, dottedName) {
  let value = parseAttestation(path.resolve(file));
  for (const component of dottedName.split(".")) {
    value = value?.[component];
  }
  if (
    typeof value !== "string" &&
    typeof value !== "number" &&
    typeof value !== "boolean"
  ) {
    fail(`field is missing or not scalar: ${dottedName}`);
  }
  process.stdout.write(String(value));
}

const [command, ...rest] = process.argv.slice(2);
if (command === "create") {
  create(parseArgs(rest));
} else if (command === "verify") {
  verify(parseArgs(rest));
} else if (command === "field" && rest.length === 2) {
  field(rest[0], rest[1]);
} else {
  process.stderr.write(
    "usage: attestation.mjs create|verify [OPTIONS]\n" +
      "       attestation.mjs field FILE DOTTED_FIELD\n",
  );
  process.exit(2);
}
