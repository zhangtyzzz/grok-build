#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { assertTargetArtifact } from "./artifact-format.mjs";

const root = fs.mkdtempSync(path.join(os.tmpdir(), "grok-artifact-format-"));

function writeMachO(file, cpu, minimumOs = 0x000b0000) {
  const body = Buffer.alloc(4096);
  body.writeUInt32LE(0xfeedfacf, 0);
  body.writeUInt32LE(cpu, 4);
  body.writeUInt32LE(3, 8);
  body.writeUInt32LE(2, 12);
  body.writeUInt32LE(3, 16);
  body.writeUInt32LE(120, 20);
  body.writeUInt32LE(0x00200085, 24);
  body.writeUInt32LE(0x19, 32);
  body.writeUInt32LE(72, 36);
  Buffer.from("__TEXT").copy(body, 40);
  body.writeBigUInt64LE(0x100000000n, 56);
  body.writeBigUInt64LE(4096n, 64);
  body.writeBigUInt64LE(0n, 72);
  body.writeBigUInt64LE(4096n, 80);
  body.writeUInt32LE(7, 88);
  body.writeUInt32LE(5, 92);
  body.writeUInt32LE(0, 96);
  body.writeUInt32LE(0, 100);
  body.writeUInt32LE(0x80000028, 104);
  body.writeUInt32LE(24, 108);
  body.writeBigUInt64LE(160n, 112);
  body.writeUInt32LE(0x32, 128);
  body.writeUInt32LE(24, 132);
  body.writeUInt32LE(1, 136);
  body.writeUInt32LE(minimumOs, 140);
  body.writeUInt32LE(minimumOs, 144);
  body.writeUInt32LE(0, 148);
  fs.writeFileSync(file, body);
}

function writeElf(file, machine) {
  const body = Buffer.alloc(4096);
  Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]).copy(body);
  body.writeUInt16LE(3, 16);
  body.writeUInt16LE(machine, 18);
  body.writeUInt32LE(1, 20);
  body.writeBigUInt64LE(0x400080n, 24);
  body.writeBigUInt64LE(64n, 32);
  body.writeUInt16LE(64, 52);
  body.writeUInt16LE(56, 54);
  body.writeUInt16LE(1, 56);
  body.writeUInt16LE(64, 58);
  body.writeUInt32LE(1, 64);
  body.writeUInt32LE(5, 68);
  body.writeBigUInt64LE(0n, 72);
  body.writeBigUInt64LE(0x400000n, 80);
  body.writeBigUInt64LE(0x400000n, 88);
  body.writeBigUInt64LE(BigInt(body.length), 96);
  body.writeBigUInt64LE(BigInt(body.length), 104);
  body.writeBigUInt64LE(4096n, 112);
  fs.writeFileSync(file, body);
}

function writePe(file, machine) {
  const body = Buffer.alloc(1024);
  body[0] = 0x4d;
  body[1] = 0x5a;
  body.writeUInt32LE(128, 0x3c);
  Buffer.from([0x50, 0x45, 0, 0]).copy(body, 128);
  body.writeUInt16LE(machine, 132);
  body.writeUInt16LE(1, 134);
  body.writeUInt16LE(240, 148);
  body.writeUInt16LE(0x0022, 150);
  const optional = 152;
  body.writeUInt16LE(0x20b, optional);
  body.writeUInt32LE(0x1000, optional + 16);
  body.writeUInt32LE(0x1000, optional + 32);
  body.writeUInt32LE(0x200, optional + 36);
  body.writeUInt32LE(0x2000, optional + 56);
  body.writeUInt32LE(0x200, optional + 60);
  body.writeUInt32LE(16, optional + 108);
  const section = optional + 240;
  Buffer.from(".text").copy(body, section);
  body.writeUInt32LE(0x100, section + 8);
  body.writeUInt32LE(0x1000, section + 12);
  body.writeUInt32LE(0x200, section + 16);
  body.writeUInt32LE(0x200, section + 20);
  body.writeUInt32LE(0x60000020, section + 36);
  fs.writeFileSync(file, body);
}

try {
  const armMac = path.join(root, "arm-mac");
  const x64Linux = path.join(root, "x64-linux");
  const armWindows = path.join(root, "arm-windows.exe");
  writeMachO(armMac, 0x0100000c);
  writeElf(x64Linux, 62);
  writePe(armWindows, 0xaa64);

  assert.doesNotThrow(() =>
    assertTargetArtifact("aarch64-apple-darwin", armMac),
  );
  assert.doesNotThrow(() =>
    assertTargetArtifact("x86_64-unknown-linux-gnu", x64Linux),
  );
  assert.doesNotThrow(() =>
    assertTargetArtifact("aarch64-pc-windows-msvc", armWindows),
  );
  assert.throws(
    () => assertTargetArtifact("x86_64-apple-darwin", armMac),
    /CPU type/,
  );
  assert.throws(
    () => assertTargetArtifact("x86_64-pc-windows-msvc", x64Linux),
    /DOS\/PE/,
  );

  const machStub = path.join(root, "mach-stub");
  const machLibrary = path.join(root, "mach-library");
  const machNonExecutableSegment = path.join(root, "mach-non-exec-segment");
  const machMissingMinimum = path.join(root, "mach-missing-minimum");
  const machNewerMinimum = path.join(root, "mach-newer-minimum");
  fs.writeFileSync(machStub, fs.readFileSync(armMac).subarray(0, 32));
  writeMachO(machLibrary, 0x0100000c);
  const machLibraryBody = fs.readFileSync(machLibrary);
  machLibraryBody.writeUInt32LE(6, 12);
  fs.writeFileSync(machLibrary, machLibraryBody);
  writeMachO(machNonExecutableSegment, 0x0100000c);
  const machNonExecutableBody = fs.readFileSync(machNonExecutableSegment);
  machNonExecutableBody.writeUInt32LE(1, 92);
  fs.writeFileSync(machNonExecutableSegment, machNonExecutableBody);
  writeMachO(machMissingMinimum, 0x0100000c);
  const machMissingMinimumBody = fs.readFileSync(machMissingMinimum);
  machMissingMinimumBody.writeUInt32LE(2, 16);
  machMissingMinimumBody.writeUInt32LE(96, 20);
  fs.writeFileSync(machMissingMinimum, machMissingMinimumBody);
  writeMachO(machNewerMinimum, 0x0100000c, 0x000c0000);
  assert.throws(
    () => assertTargetArtifact("aarch64-apple-darwin", machStub),
    /load-command table/,
  );
  assert.throws(
    () => assertTargetArtifact("aarch64-apple-darwin", machLibrary),
    /MH_EXECUTE/,
  );
  assert.throws(
    () =>
      assertTargetArtifact(
        "aarch64-apple-darwin",
        machNonExecutableSegment,
      ),
    /no executable entry/,
  );
  assert.throws(
    () => assertTargetArtifact("aarch64-apple-darwin", machMissingMinimum),
    /does not declare a minimum macOS version/,
  );
  assert.throws(
    () => assertTargetArtifact("aarch64-apple-darwin", machNewerMinimum),
    /requires macOS 12\.0\.0, newer than the supported 11\.0\.0/,
  );

  const elfStub = path.join(root, "elf-stub");
  const elfObject = path.join(root, "elf-object");
  const elfNonExecutableSegment = path.join(root, "elf-non-exec-segment");
  fs.writeFileSync(elfStub, fs.readFileSync(x64Linux).subarray(0, 64));
  writeElf(elfObject, 62);
  const elfObjectBody = fs.readFileSync(elfObject);
  elfObjectBody.writeUInt16LE(1, 16);
  fs.writeFileSync(elfObject, elfObjectBody);
  writeElf(elfNonExecutableSegment, 62);
  const elfNonExecutableBody = fs.readFileSync(elfNonExecutableSegment);
  elfNonExecutableBody.writeUInt32LE(4, 68);
  fs.writeFileSync(elfNonExecutableSegment, elfNonExecutableBody);
  assert.throws(
    () => assertTargetArtifact("x86_64-unknown-linux-gnu", elfStub),
    /program-header table/,
  );
  assert.throws(
    () => assertTargetArtifact("x86_64-unknown-linux-gnu", elfObject),
    /ET_EXEC or ET_DYN/,
  );
  assert.throws(
    () =>
      assertTargetArtifact(
        "x86_64-unknown-linux-gnu",
        elfNonExecutableSegment,
      ),
    /not in an executable PT_LOAD/,
  );

  const peStub = path.join(root, "pe-stub.exe");
  const peLibrary = path.join(root, "pe-library.dll");
  const peNonExecutableSection = path.join(root, "pe-non-exec-section.exe");
  fs.writeFileSync(peStub, fs.readFileSync(armWindows).subarray(0, 152));
  writePe(peLibrary, 0xaa64);
  const peLibraryBody = fs.readFileSync(peLibrary);
  peLibraryBody.writeUInt16LE(0x2022, 150);
  fs.writeFileSync(peLibrary, peLibraryBody);
  writePe(peNonExecutableSection, 0xaa64);
  const peNonExecutableBody = fs.readFileSync(peNonExecutableSection);
  peNonExecutableBody.writeUInt32LE(0x40000020, 428);
  fs.writeFileSync(peNonExecutableSection, peNonExecutableBody);
  assert.throws(
    () => assertTargetArtifact("aarch64-pc-windows-msvc", peStub),
    /section table is outside the file/,
  );
  assert.throws(
    () => assertTargetArtifact("aarch64-pc-windows-msvc", peLibrary),
    /executable image/,
  );
  assert.throws(
    () =>
      assertTargetArtifact("aarch64-pc-windows-msvc", peNonExecutableSection),
    /not in an executable section/,
  );
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

process.stdout.write("target artifact format tests passed\n");
