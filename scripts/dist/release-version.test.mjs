import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { releaseVersion, libraries } from "./release-version.mjs";

function fixture(t, version = "1.0.47-alpha.3") {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "grok-release-version-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  let lock = "version = 4\n";
  for (const name of ["xai-grok-pager-bin", ...libraries]) {
    const v = name === "xai-grok-pager-bin" ? version : "0.0.0";
    const dir = path.join(root, "crates/codegen", name);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "${v}"\n[dependencies]\n`);
    lock += `\n[[package]]\nname = "${name}"\nversion = "${v}"\n`;
  }
  fs.writeFileSync(path.join(root, "Cargo.lock"), lock);
  return root;
}

test("stable and alpha version changes leave library versions fixed", t => {
  for (const version of ["1.0.46", "1.0.47-alpha.3", "1.0.47-alpha.4", "1.0.47"]) {
    assert.equal(releaseVersion(fixture(t, version), version), version);
  }
});
test("reject mismatched release tag", t => {
  assert.throws(() => releaseVersion(fixture(t), "1.0.47-alpha.4"), /does not match binary/);
});
test("reject accidentally bumped library", t => {
  const root = fixture(t);
  const file = path.join(root, "crates/codegen/xai-grok-shell/Cargo.toml");
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace('version = "0.0.0"', 'version = "1.0.47"'));
  assert.throws(() => releaseVersion(root), /must retain internal version/);
});
test("reject stale or duplicate lockfile package", t => {
  for (const duplicate of [false, true]) {
    const root = fixture(t);
    const file = path.join(root, "Cargo.lock");
    const lock = fs.readFileSync(file, "utf8");
    fs.writeFileSync(file, duplicate ? lock + '\n[[package]]\nname = "xai-grok-shell"\nversion = "0.0.0"\n' : lock.replace('version = "1.0.47-alpha.3"', 'version = "1.0.47-alpha.2"'));
    assert.throws(() => releaseVersion(root), /Cargo.lock does not match/);
  }
});
test("reject missing or malformed version metadata", t => {
  for (const version of ["", "01.0.47", "1.0.47-alpha.03", "1.0.47-dev"]) {
    assert.throws(() => releaseVersion(fixture(t, version)));
  }
  const root = fixture(t);
  fs.rmSync(path.join(root, "crates/codegen/xai-grok-version/Cargo.toml"));
  assert.throws(() => releaseVersion(root), /ENOENT/);
});
