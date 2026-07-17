#!/usr/bin/env node

import fs from "node:fs";
import { fileURLToPath } from "node:url";

const TARGET_FORMATS = {
  "aarch64-apple-darwin": {
    format: "Mach-O",
    cpu: 0x0100000c,
    maximumMinimumOs: 0x000b0000,
  },
  "x86_64-apple-darwin": {
    format: "Mach-O",
    cpu: 0x01000007,
    maximumMinimumOs: 0x000b0000,
  },
  "aarch64-unknown-linux-gnu": { format: "ELF", machine: 183 },
  "x86_64-unknown-linux-gnu": { format: "ELF", machine: 62 },
  "aarch64-pc-windows-msvc": { format: "PE", machine: 0xaa64 },
  "x86_64-pc-windows-msvc": { format: "PE", machine: 0x8664 },
};

const MAX_HEADER_BYTES = 16 * 1024 * 1024;

function fail(message) {
  throw new Error(`target artifact validation failed: ${message}`);
}

function readPrefix(file, fileSize) {
  const length = Math.min(fileSize, MAX_HEADER_BYTES);
  const fd = fs.openSync(file, "r");
  try {
    const buffer = Buffer.alloc(length);
    const bytesRead = fs.readSync(fd, buffer, 0, length, 0);
    return buffer.subarray(0, bytesRead);
  } finally {
    fs.closeSync(fd);
  }
}

function rangeFits(offset, size, limit) {
  return (
    Number.isSafeInteger(offset) &&
    Number.isSafeInteger(size) &&
    offset >= 0 &&
    size >= 0 &&
    offset <= limit &&
    size <= limit - offset
  );
}

function bigRangeFits(offset, size, limit) {
  const upper = BigInt(limit);
  return offset >= 0n && size >= 0n && offset <= upper && size <= upper - offset;
}

function assertHeaderAvailable(end, buffer, target, label) {
  if (!Number.isSafeInteger(end) || end > buffer.length) {
    fail(
      `${target} ${label} is unreasonably large or outside the readable header`,
    );
  }
}

function assertMachO(buffer, fileSize, expected, target) {
  const MACH_HEADER_64_SIZE = 32;
  const LC_SEGMENT_64 = 0x19;
  const LC_UNIXTHREAD = 0x5;
  const LC_MAIN = 0x80000028;
  const LC_VERSION_MIN_MACOSX = 0x24;
  const LC_BUILD_VERSION = 0x32;
  const PLATFORM_MACOS = 1;
  if (
    buffer.length < MACH_HEADER_64_SIZE ||
    buffer.readUInt32LE(0) !== 0xfeedfacf
  ) {
    fail(`${target} must be a little-endian 64-bit Mach-O executable`);
  }
  if (buffer.readUInt32LE(4) !== expected.cpu) {
    fail(`${target} Mach-O CPU type does not match the target architecture`);
  }
  if (buffer.readUInt32LE(12) !== 2) {
    fail(`${target} Mach-O file type is not MH_EXECUTE`);
  }

  const commandCount = buffer.readUInt32LE(16);
  const commandBytes = buffer.readUInt32LE(20);
  if (
    commandCount === 0 ||
    commandCount > 65535 ||
    commandBytes < commandCount * 8 ||
    !rangeFits(MACH_HEADER_64_SIZE, commandBytes, fileSize)
  ) {
    fail(`${target} has an invalid Mach-O load-command table`);
  }
  const commandEnd = MACH_HEADER_64_SIZE + commandBytes;
  assertHeaderAvailable(commandEnd, buffer, target, "Mach-O load-command table");

  let cursor = MACH_HEADER_64_SIZE;
  const executableFileRanges = [];
  let hasEntryCommand = false;
  let mainEntryOffset = null;
  const minimumOsVersions = [];
  for (let index = 0; index < commandCount; index += 1) {
    if (cursor + 8 > commandEnd) {
      fail(`${target} has a truncated Mach-O load command`);
    }
    const command = buffer.readUInt32LE(cursor);
    const commandSize = buffer.readUInt32LE(cursor + 4);
    if (
      commandSize < 8 ||
      commandSize % 8 !== 0 ||
      !rangeFits(cursor, commandSize, commandEnd)
    ) {
      fail(`${target} has an invalid Mach-O load-command size`);
    }

    if (command === LC_SEGMENT_64) {
      if (commandSize < 72) {
        fail(`${target} has a truncated LC_SEGMENT_64 command`);
      }
      const sectionCount = buffer.readUInt32LE(cursor + 64);
      if (72 + sectionCount * 80 > commandSize) {
        fail(`${target} has an invalid LC_SEGMENT_64 section table`);
      }
      const fileOffset = buffer.readBigUInt64LE(cursor + 40);
      const segmentSize = buffer.readBigUInt64LE(cursor + 48);
      const initialProtection = buffer.readUInt32LE(cursor + 60);
      if (!bigRangeFits(fileOffset, segmentSize, fileSize)) {
        fail(`${target} has a Mach-O segment outside the file`);
      }
      if (segmentSize > 0n && (initialProtection & 0x4) !== 0) {
        executableFileRanges.push([fileOffset, fileOffset + segmentSize]);
      }
    } else if (command === LC_MAIN) {
      if (commandSize < 24) {
        fail(`${target} has a truncated LC_MAIN command`);
      }
      const entryOffset = buffer.readBigUInt64LE(cursor + 8);
      if (entryOffset >= BigInt(fileSize)) {
        fail(`${target} Mach-O entry point is outside the file`);
      }
      mainEntryOffset = entryOffset;
      hasEntryCommand = true;
    } else if (command === LC_UNIXTHREAD) {
      if (commandSize <= 16) {
        fail(`${target} has a truncated LC_UNIXTHREAD command`);
      }
      hasEntryCommand = true;
    } else if (command === LC_VERSION_MIN_MACOSX) {
      if (commandSize !== 16) {
        fail(`${target} has an invalid LC_VERSION_MIN_MACOSX command`);
      }
      minimumOsVersions.push(buffer.readUInt32LE(cursor + 8));
    } else if (command === LC_BUILD_VERSION) {
      if (commandSize < 24) {
        fail(`${target} has a truncated LC_BUILD_VERSION command`);
      }
      const platform = buffer.readUInt32LE(cursor + 8);
      const toolCount = buffer.readUInt32LE(cursor + 20);
      if (
        platform !== PLATFORM_MACOS ||
        commandSize !== 24 + toolCount * 8
      ) {
        fail(`${target} has an invalid macOS LC_BUILD_VERSION command`);
      }
      minimumOsVersions.push(buffer.readUInt32LE(cursor + 12));
    }
    cursor += commandSize;
  }
  if (cursor !== commandEnd) {
    fail(`${target} Mach-O load-command sizes do not match sizeofcmds`);
  }
  if (executableFileRanges.length === 0 || !hasEntryCommand) {
    fail(`${target} Mach-O image has no executable entry or file-backed segment`);
  }
  if (minimumOsVersions.length === 0) {
    fail(`${target} Mach-O image does not declare a minimum macOS version`);
  }
  if (
    minimumOsVersions.some(
      (version) => version === 0 || version !== minimumOsVersions[0],
    )
  ) {
    fail(`${target} Mach-O image has invalid or conflicting macOS versions`);
  }
  if (minimumOsVersions[0] > expected.maximumMinimumOs) {
    fail(
      `${target} requires macOS ${formatPackedVersion(minimumOsVersions[0])}, ` +
        `newer than the supported ${formatPackedVersion(expected.maximumMinimumOs)}`,
    );
  }
  if (
    mainEntryOffset !== null &&
    !executableFileRanges.some(
      ([start, end]) => mainEntryOffset >= start && mainEntryOffset < end,
    )
  ) {
    fail(`${target} Mach-O entry point is not in an executable segment`);
  }
}

function formatPackedVersion(version) {
  return `${version >>> 16}.${(version >>> 8) & 0xff}.${version & 0xff}`;
}

function assertElf(buffer, fileSize, expected, target) {
  const ELF_HEADER_SIZE = 64;
  const PROGRAM_HEADER_SIZE = 56;
  const PT_LOAD = 1;
  const PF_X = 1;
  if (
    buffer.length < ELF_HEADER_SIZE ||
    !buffer.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])) ||
    buffer[4] !== 2 ||
    buffer[5] !== 1 ||
    buffer[6] !== 1
  ) {
    fail(`${target} must be a little-endian 64-bit ELF executable`);
  }
  if (buffer.readUInt16LE(18) !== expected.machine) {
    fail(`${target} ELF machine does not match the target architecture`);
  }
  const fileType = buffer.readUInt16LE(16);
  if (fileType !== 2 && fileType !== 3) {
    fail(`${target} ELF file type must be ET_EXEC or ET_DYN`);
  }
  if (buffer.readUInt32LE(20) !== 1 || buffer.readUInt16LE(52) !== 64) {
    fail(`${target} has an invalid ELF64 header`);
  }

  const entry = buffer.readBigUInt64LE(24);
  const programOffset = buffer.readBigUInt64LE(32);
  const programEntrySize = buffer.readUInt16LE(54);
  const programCount = buffer.readUInt16LE(56);
  if (
    entry === 0n ||
    programEntrySize !== PROGRAM_HEADER_SIZE ||
    programCount === 0 ||
    programCount === 0xffff ||
    programOffset < BigInt(ELF_HEADER_SIZE)
  ) {
    fail(`${target} has an invalid ELF64 program-header table or entry point`);
  }
  const programBytes = BigInt(programEntrySize) * BigInt(programCount);
  if (!bigRangeFits(programOffset, programBytes, fileSize)) {
    fail(`${target} ELF64 program-header table is outside the file`);
  }
  if (
    programOffset > BigInt(Number.MAX_SAFE_INTEGER) ||
    programBytes > BigInt(Number.MAX_SAFE_INTEGER)
  ) {
    fail(`${target} ELF64 program-header table is unreasonably large`);
  }
  const programStart = Number(programOffset);
  const programEnd = programStart + Number(programBytes);
  assertHeaderAvailable(
    programEnd,
    buffer,
    target,
    "ELF64 program-header table",
  );

  let entryInExecutableLoad = false;
  let hasLoadSegment = false;
  for (let index = 0; index < programCount; index += 1) {
    const offset = programStart + index * programEntrySize;
    const type = buffer.readUInt32LE(offset);
    const flags = buffer.readUInt32LE(offset + 4);
    const fileOffset = buffer.readBigUInt64LE(offset + 8);
    const virtualAddress = buffer.readBigUInt64LE(offset + 16);
    const fileBytes = buffer.readBigUInt64LE(offset + 32);
    const memoryBytes = buffer.readBigUInt64LE(offset + 40);
    if (!bigRangeFits(fileOffset, fileBytes, fileSize)) {
      fail(`${target} has an ELF64 program segment outside the file`);
    }
    if (type !== PT_LOAD) {
      continue;
    }
    hasLoadSegment = true;
    if (fileBytes > memoryBytes) {
      fail(`${target} has an invalid ELF64 PT_LOAD size`);
    }
    if (
      (flags & PF_X) !== 0 &&
      fileBytes > 0n &&
      entry >= virtualAddress &&
      entry < virtualAddress + fileBytes
    ) {
      entryInExecutableLoad = true;
    }
  }
  if (!hasLoadSegment || !entryInExecutableLoad) {
    fail(`${target} ELF64 entry point is not in an executable PT_LOAD segment`);
  }
}

function assertPe(buffer, fileSize, expected, target) {
  const COFF_HEADER_SIZE = 20;
  const PE32_PLUS_MINIMUM_SIZE = 112;
  const SECTION_HEADER_SIZE = 40;
  const IMAGE_FILE_EXECUTABLE_IMAGE = 0x0002;
  const IMAGE_FILE_DLL = 0x2000;
  const IMAGE_SCN_MEM_EXECUTE = 0x20000000;
  if (buffer.length < 0x40 || buffer[0] !== 0x4d || buffer[1] !== 0x5a) {
    fail(`${target} must have a DOS/PE executable header`);
  }
  const peOffset = buffer.readUInt32LE(0x3c);
  if (
    peOffset < 0x40 ||
    !rangeFits(peOffset, 4 + COFF_HEADER_SIZE, fileSize)
  ) {
    fail(`${target} has an invalid PE header offset`);
  }
  assertHeaderAvailable(
    peOffset + 4 + COFF_HEADER_SIZE,
    buffer,
    target,
    "PE/COFF header",
  );
  if (
    !buffer
      .subarray(peOffset, peOffset + 4)
      .equals(Buffer.from([0x50, 0x45, 0, 0]))
  ) {
    fail(`${target} has an invalid PE signature`);
  }
  if (buffer.readUInt16LE(peOffset + 4) !== expected.machine) {
    fail(`${target} PE machine does not match the target architecture`);
  }

  const coffOffset = peOffset + 4;
  const sectionCount = buffer.readUInt16LE(coffOffset + 2);
  const optionalSize = buffer.readUInt16LE(coffOffset + 16);
  const characteristics = buffer.readUInt16LE(coffOffset + 18);
  if (
    sectionCount === 0 ||
    sectionCount > 96 ||
    optionalSize < PE32_PLUS_MINIMUM_SIZE ||
    (characteristics & IMAGE_FILE_EXECUTABLE_IMAGE) === 0 ||
    (characteristics & IMAGE_FILE_DLL) !== 0
  ) {
    fail(`${target} PE/COFF header does not describe an executable image`);
  }

  const optionalOffset = coffOffset + COFF_HEADER_SIZE;
  const sectionOffset = optionalOffset + optionalSize;
  const sectionBytes = sectionCount * SECTION_HEADER_SIZE;
  if (
    !rangeFits(optionalOffset, optionalSize, fileSize) ||
    !rangeFits(sectionOffset, sectionBytes, fileSize)
  ) {
    fail(`${target} PE optional header or section table is outside the file`);
  }
  assertHeaderAvailable(
    sectionOffset + sectionBytes,
    buffer,
    target,
    "PE optional header and section table",
  );
  if (buffer.readUInt16LE(optionalOffset) !== 0x20b) {
    fail(`${target} PE executable must use the PE32+ optional header`);
  }
  const entryRva = buffer.readUInt32LE(optionalOffset + 16);
  const sectionAlignment = buffer.readUInt32LE(optionalOffset + 32);
  const fileAlignment = buffer.readUInt32LE(optionalOffset + 36);
  const imageSize = buffer.readUInt32LE(optionalOffset + 56);
  const headerSize = buffer.readUInt32LE(optionalOffset + 60);
  const directoryCount = buffer.readUInt32LE(optionalOffset + 108);
  if (
    entryRva === 0 ||
    sectionAlignment === 0 ||
    fileAlignment < 512 ||
    fileAlignment > 65536 ||
    (fileAlignment & (fileAlignment - 1)) !== 0 ||
    sectionAlignment < fileAlignment ||
    imageSize === 0 ||
    headerSize < sectionOffset + sectionBytes ||
    headerSize > fileSize ||
    headerSize % fileAlignment !== 0 ||
    imageSize % sectionAlignment !== 0 ||
    directoryCount > 128 ||
    PE32_PLUS_MINIMUM_SIZE + directoryCount * 8 > optionalSize
  ) {
    fail(`${target} has an invalid PE32+ optional header`);
  }

  let entryInExecutableSection = false;
  for (let index = 0; index < sectionCount; index += 1) {
    const offset = sectionOffset + index * SECTION_HEADER_SIZE;
    const virtualSize = buffer.readUInt32LE(offset + 8);
    const virtualAddress = buffer.readUInt32LE(offset + 12);
    const rawSize = buffer.readUInt32LE(offset + 16);
    const rawOffset = buffer.readUInt32LE(offset + 20);
    const sectionCharacteristics = buffer.readUInt32LE(offset + 36);
    if (
      virtualAddress % sectionAlignment !== 0 ||
      virtualAddress > imageSize ||
      Math.max(virtualSize, rawSize) > imageSize - virtualAddress ||
      (rawSize > 0 &&
        (rawOffset < headerSize ||
          rawOffset % fileAlignment !== 0 ||
          rawSize % fileAlignment !== 0 ||
          !rangeFits(rawOffset, rawSize, fileSize)))
    ) {
      fail(`${target} has a malformed or out-of-file PE section`);
    }
    const mappedSize = Math.max(virtualSize, rawSize);
    if (
      mappedSize > 0 &&
      entryRva >= virtualAddress &&
      entryRva - virtualAddress < mappedSize &&
      (sectionCharacteristics & IMAGE_SCN_MEM_EXECUTE) !== 0
    ) {
      entryInExecutableSection = true;
    }
  }
  if (!entryInExecutableSection) {
    fail(`${target} PE entry point is not in an executable section`);
  }
}

export function assertTargetArtifact(target, file) {
  const expected = TARGET_FORMATS[target];
  if (!expected) {
    fail(`unsupported target: ${target}`);
  }
  const stat = fs.statSync(file);
  if (!stat.isFile()) {
    fail(`not a regular file: ${file}`);
  }
  const buffer = readPrefix(file, stat.size);
  switch (expected.format) {
    case "Mach-O":
      assertMachO(buffer, stat.size, expected, target);
      break;
    case "ELF":
      assertElf(buffer, stat.size, expected, target);
      break;
    case "PE":
      assertPe(buffer, stat.size, expected, target);
      break;
    default:
      fail(`unsupported artifact format: ${expected.format}`);
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 4) {
    process.stderr.write("usage: artifact-format.mjs TARGET BINARY\n");
    process.exit(2);
  }
  assertTargetArtifact(process.argv[2], process.argv[3]);
}
