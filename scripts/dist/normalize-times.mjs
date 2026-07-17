#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const [rootArg, epochArg] = process.argv.slice(2);
const epoch = Number(epochArg);
if (!rootArg || !Number.isSafeInteger(epoch) || epoch < 0) {
  process.stderr.write("usage: normalize-times.mjs ROOT SOURCE_DATE_EPOCH\n");
  process.exit(2);
}

const root = path.resolve(rootArg);
const entries = [];
const walk = (current) => {
  for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
    const full = path.join(current, entry.name);
    if (entry.isDirectory()) {
      walk(full);
    }
    entries.push(full);
  }
};
walk(root);
entries.push(root);

const when = new Date(epoch * 1000);
for (const entry of entries) {
  fs.utimesSync(entry, when, when);
}
