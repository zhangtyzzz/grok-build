#!/usr/bin/env node

let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  input += chunk;
});
process.stdin.on("end", () => {
  const entries = input.split(/\r?\n/).filter(Boolean);
  if (entries.length === 0) {
    throw new Error("archive contains no entries");
  }
  const normalizedEntries = new Set();
  for (const raw of entries) {
    const entry = raw.replaceAll("\\", "/");
    const components = entry.split("/");
    if (components.at(-1) === "") {
      components.pop();
    }
    if (
      entry.startsWith("/") ||
      /^[A-Za-z]:\//.test(entry) ||
      components.length === 0 ||
      components.some((component) => component.length === 0) ||
      components.includes(".") ||
      components.includes("..") ||
      entry.includes("\0")
    ) {
      throw new Error(`unsafe archive path: ${JSON.stringify(raw)}`);
    }
    const normalized = components.join("/");
    if (normalizedEntries.has(normalized)) {
      throw new Error(
        `duplicate normalized archive path: ${JSON.stringify(raw)}`,
      );
    }
    normalizedEntries.add(normalized);
  }
});
