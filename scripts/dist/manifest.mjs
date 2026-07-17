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

function parseArgs(argv) {
  const values = { bundledTools: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i];
    if (!key.startsWith("--")) {
      throw new Error(`unexpected argument: ${key}`);
    }
    const name = key.slice(2);
    const value = argv[++i];
    if (value === undefined) {
      throw new Error(`missing value for ${key}`);
    }
    if (name === "bundled-tool") {
      values.bundledTools.push(value);
    } else {
      values[name] = value;
    }
  }
  return values;
}

function requireString(args, name) {
  const value = args[name];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`--${name} is required`);
  }
  return value;
}

function requireBoolean(args, name) {
  const value = requireString(args, name);
  if (value !== "true" && value !== "false") {
    throw new Error(`--${name} must be true or false`);
  }
  return value === "true";
}

function sha256File(file) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

function relativePath(root, file) {
  return path.relative(root, file).split(path.sep).join("/");
}

function payloadFiles(root, excluded = new Set()) {
  return walkFiles(root)
    .map((file) => ({
      file,
      path: relativePath(root, file),
    }))
    .filter((entry) => !excluded.has(entry.path))
    .sort((a, b) => a.path.localeCompare(b.path, "en"));
}

function fileRecords(root, excluded) {
  return payloadFiles(root, excluded).map(({ file, path: relative }) => {
    const stat = fs.statSync(file);
    return {
      path: relative,
      size: stat.size,
      sha256: sha256File(file),
      executable: (stat.mode & 0o111) !== 0,
    };
  });
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

function parseBundledTool(spec) {
  const first = spec.indexOf(",");
  const second = spec.indexOf(",", first + 1);
  if (first <= 0 || second <= first + 1 || second === spec.length - 1) {
    throw new Error(
      `invalid --bundled-tool ${JSON.stringify(spec)}; expected name,version,path`,
    );
  }
  const name = spec.slice(0, first);
  const version = spec.slice(first + 1, second);
  const file = spec.slice(second + 1);
  if (!fs.statSync(file).isFile()) {
    throw new Error(`bundled tool is not a regular file: ${file}`);
  }
  const stat = fs.statSync(file);
  return {
    name,
    version,
    size: stat.size,
    sha256: sha256File(file),
  };
}

function writeInternalChecksums(root) {
  const records = fileRecords(root, new Set(["MANIFEST.sha256"]));
  const body = records
    .map((record) => `${record.sha256}  ${record.path}`)
    .join("\n");
  fs.writeFileSync(path.join(root, "MANIFEST.sha256"), `${body}\n`, "utf8");
}

const args = parseArgs(process.argv.slice(2));
const root = path.resolve(requireString(args, "stage-dir"));
const version = requireString(args, "version");
const target = requireString(args, "target");
const gitCommit = requireString(args, "git-commit");
const sourceRev = requireString(args, "source-rev");
const sourceDateEpoch = Number(requireString(args, "source-date-epoch"));
const executable = requireString(args, "executable");
const dirty = requireBoolean(args, "dirty");
const buildDirty = requireBoolean(args, "build-dirty");
const attestationVerified = requireBoolean(args, "attestation-verified");

if (!/^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z._-]+)?$/.test(version)) {
  throw new Error(`invalid distribution version: ${version}`);
}
if (!Object.hasOwn(targets, target)) {
  throw new Error(`unsupported distribution target: ${target}`);
}
if (targets[target].executable !== executable) {
  throw new Error(
    `executable ${executable} does not match target ${target} (${targets[target].executable})`,
  );
}
if (!/^[0-9a-f]{40,64}$/i.test(gitCommit)) {
  throw new Error(`invalid git commit: ${gitCommit}`);
}
if (!/^[0-9a-f]{40,64}$/i.test(sourceRev)) {
  throw new Error(`invalid SOURCE_REV: ${sourceRev}`);
}
if (!Number.isSafeInteger(sourceDateEpoch) || sourceDateEpoch < 0) {
  throw new Error(`invalid source date epoch: ${args["source-date-epoch"]}`);
}
if (!fs.statSync(root).isDirectory()) {
  throw new Error(`stage directory does not exist: ${root}`);
}

const artifactPath = `bin/${executable}`;
const artifactFile = path.join(root, "bin", executable);
if (!fs.statSync(artifactFile).isFile()) {
  throw new Error(`staged executable does not exist: ${artifactFile}`);
}

assertPayloadPolicy(root);

const records = fileRecords(
  root,
  new Set(["build-manifest.json", "MANIFEST.sha256"]),
);
const artifact = records.find((record) => record.path === artifactPath);
if (!artifact) {
  throw new Error(`staged executable is missing from payload records: ${artifactPath}`);
}
const attestationRecord = records.find(
  (record) => record.path === "build-attestation.json",
);
let buildAttestation = null;
let attestedBundledTools = null;
if (attestationVerified) {
  if (!attestationRecord) {
    throw new Error("verified build attestation is missing from the staged payload");
  }
  const attestation = JSON.parse(
    fs.readFileSync(path.join(root, "build-attestation.json"), "utf8"),
  );
  if (
    attestation.schemaVersion !== 1 ||
    attestation.product !== "grok-build" ||
    attestation.version !== version ||
    attestation.target !== target ||
    attestation.source?.gitCommit !== gitCommit ||
    attestation.source?.sourceRev !== sourceRev ||
    attestation.artifact?.sha256 !== artifact.sha256 ||
    attestation.artifact?.size !== artifact.size
  ) {
    throw new Error("build attestation does not match the staged artifact/source");
  }
  assertTargetArtifact(target, artifactFile);
  buildAttestation = {
    path: attestationRecord.path,
    sha256: attestationRecord.sha256,
    schemaVersion: attestation.schemaVersion,
  };
  attestedBundledTools = attestation.bundledTools;
} else if (attestationRecord) {
  throw new Error("unverified build attestation must not be staged");
}

const bundledTools = args.bundledTools
  .map(parseBundledTool)
  .sort((a, b) => a.name.localeCompare(b.name, "en"));
if (
  attestationVerified &&
  JSON.stringify(attestedBundledTools) !== JSON.stringify(bundledTools)
) {
  throw new Error("bundled-tool metadata does not match the build attestation");
}
const requiredTools =
  targets[target].platform === "windows"
    ? ["ripgrep"]
    : ["bfs", "ripgrep", "ugrep"];
const bundledToolNames = new Set(bundledTools.map((tool) => tool.name));
const hasAllRequiredTools = requiredTools.every((name) =>
  bundledToolNames.has(name),
);
const profileRecords = records.filter((record) =>
  record.path.startsWith("profiles/starter/"),
);
if (profileRecords.length === 0) {
  throw new Error("starter profile is empty");
}

const manifest = {
  schemaVersion: 1,
  product: "grok-build",
  version,
  releaseReady:
    !dirty && !buildDirty && hasAllRequiredTools && attestationVerified,
  source: {
    gitCommit,
    sourceRev,
    dirty,
    buildDirty,
  },
  build: {
    target,
    profile: requireString(args, "profile"),
    features: requireString(args, "features")
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean)
      .sort(),
    rustc: requireString(args, "rustc"),
    cargo: requireString(args, "cargo"),
    sourceDateEpoch,
    timestamp: new Date(sourceDateEpoch * 1000).toISOString(),
  },
  artifact: {
    path: artifact.path,
    size: artifact.size,
    sha256: artifact.sha256,
  },
  buildAttestation,
  bundledTools,
  profilePack: {
    path: "profiles/starter",
    fileCount: profileRecords.length,
    sha256: treeHash(records, "profiles/starter/"),
  },
  licenses: {
    project: "LICENSE",
    thirdParty: "THIRD-PARTY-NOTICES",
    bundledTools: "BUNDLED-TOOLS-NOTICES.md",
  },
  files: records,
};

const manifestPath = path.join(root, "build-manifest.json");
fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
assertPayloadPolicy(root);
writeInternalChecksums(root);
