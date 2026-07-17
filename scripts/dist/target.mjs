#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const targets = JSON.parse(
  fs.readFileSync(path.join(here, "targets.json"), "utf8"),
);

const [target, field] = process.argv.slice(2);
if (!target || !Object.hasOwn(targets, target)) {
  process.stderr.write(
    `unsupported target ${JSON.stringify(target ?? "")}; supported targets: ${Object.keys(targets).join(", ")}\n`,
  );
  process.exit(2);
}

if (!field) {
  process.stdout.write(`${JSON.stringify(targets[target])}\n`);
  process.exit(0);
}

if (!Object.hasOwn(targets[target], field)) {
  process.stderr.write(
    `target ${target} does not define field ${JSON.stringify(field)}\n`,
  );
  process.exit(2);
}

const value = targets[target][field];
if (typeof value !== "string") {
  process.stderr.write(`target field ${field} is not a string\n`);
  process.exit(2);
}
process.stdout.write(`${value}\n`);
