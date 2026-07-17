import fs from "node:fs";
import path from "node:path";

const ASSIGNMENT =
  /(?:^|[\s{,:])(["']?)([A-Za-z_][A-Za-z0-9_.-]*)\1\s*[:=]\s*(?:"([^"\r\n]*)"|'([^'\r\n]*)'|([^\s#,\]}"'{]+))/g;
const KNOWN_SECRET =
  /\b(sk-(?:ant-|proj-)?[A-Za-z0-9_-]{12,}|gh[opsu]_[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9-]{12,}|npm_[A-Za-z0-9]{20,}|glpat-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,})\b/;
const PRIVATE_KEY_MARKER =
  /-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----/;
const UNIX_MACHINE_PATH =
  /(^|[\s("'`=])\/(?!\/)[A-Za-z0-9._-]+(?:\/[^\s)"'`,]+)*/;
const WINDOWS_MACHINE_PATH = /(^|[\s("'`=])[A-Za-z]:\\[^\s)"'`,]+/;
const UTF8_DECODER = new TextDecoder("utf-8", { fatal: true });

function walkFiles(root, directories = null) {
  const files = [];
  const walk = (current) => {
    const entries = fs
      .readdirSync(current, { withFileTypes: true })
      .sort((a, b) => a.name.localeCompare(b.name, "en"));
    for (const entry of entries) {
      const full = path.join(current, entry.name);
      if (entry.isSymbolicLink()) {
        throw new Error(`distribution payload must not contain symlinks: ${full}`);
      }
      if (entry.isDirectory()) {
        directories?.push(full);
        walk(full);
      } else if (entry.isFile()) {
        files.push(full);
      } else {
        throw new Error(`unsupported distribution payload entry: ${full}`);
      }
    }
  };
  walk(root);
  return files;
}

function isPlaceholder(value) {
  const normalized = value.trim();
  const environmentReference =
    String.raw`\$(?:[A-Za-z_][A-Za-z0-9_]*|\{[A-Za-z_][A-Za-z0-9_]*(?::-[^}]*)?\})`;
  return (
    normalized === "" ||
    /^<[^>\r\n]+>$/.test(normalized) ||
    /^env:[A-Za-z_][A-Za-z0-9_]*$/i.test(normalized) ||
    new RegExp(`^${environmentReference}$`).test(normalized) ||
    new RegExp(`^Bearer\\s+${environmentReference}$`, "i").test(normalized) ||
    /^\$(?:[0-9@*#?!$-]|\([\s\S]*\))$/.test(normalized)
  );
}

function isSecretKey(value) {
  const normalized = value
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .toLowerCase()
    .replace(/[.-]+/g, "_")
    .replace(/_+/g, "_");
  return (
    normalized === "api_key" ||
    normalized === "x_api_key" ||
    normalized === "access_token" ||
    normalized === "client_secret" ||
    normalized === "authorization" ||
    normalized === "authorization_header" ||
    normalized === "password" ||
    normalized === "secret" ||
    normalized === "token" ||
    normalized.endsWith("_api_key") ||
    normalized.endsWith("_access_token") ||
    normalized.endsWith("_client_secret") ||
    normalized.endsWith("_secret_access_key") ||
    normalized.endsWith("_private_key") ||
    normalized.endsWith("_authorization") ||
    normalized.endsWith("_authorization_header") ||
    normalized.endsWith("_password") ||
    normalized.endsWith("_secret") ||
    normalized.endsWith("_api_token") ||
    normalized.endsWith("_auth_token") ||
    normalized.endsWith("_bearer_token") ||
    normalized.endsWith("_refresh_token") ||
    normalized.endsWith("_session_token") ||
    normalized.endsWith("_id_token")
  );
}

function forbiddenUserStatePath(relative) {
  const components = relative.toLowerCase().split("/");
  const basename = components.at(-1);
  const forbiddenDirectories = new Set([
    ".aws",
    ".azure",
    ".cache",
    ".gnupg",
    ".ssh",
    "cache",
    "caches",
    "gcloud",
    "managed",
    "managed-config",
    "managed-policy",
    "managed_config",
    "managed_policy",
    "session-data",
    "sessions",
  ]);
  return (
    components.some((component) => forbiddenDirectories.has(component)) ||
    basename === ".ds_store" ||
    basename === ".env" ||
    basename === ".netrc" ||
    basename === ".npmrc" ||
    basename === ".pypirc" ||
    basename === "auth.json" ||
    basename === "credentials" ||
    basename === "credentials.json" ||
    /^auth\.(?:toml|ya?ml|db|sqlite3?)$/.test(basename) ||
    /^managed[-_](?:config|policy)(?:\.|$)/.test(basename) ||
    /^sessions?(?:\.db|\.jsonl?|\.sqlite3?)$/.test(basename) ||
    /^(?:cache|state)\.(?:db|sqlite3?)$/.test(basename)
  );
}

function utf8Text(buffer) {
  if (buffer.includes(0)) {
    return null;
  }
  try {
    return UTF8_DECODER.decode(buffer);
  } catch {
    return null;
  }
}

export function policyViolations(root) {
  const violations = [];
  const directories = [];
  const files = walkFiles(root, directories);
  for (const directory of directories) {
    const relative = path.relative(root, directory).split(path.sep).join("/");
    if (forbiddenUserStatePath(relative)) {
      violations.push(
        `${relative}/: user authentication, session, managed-policy, or cache state must not be distributed`,
      );
    }
  }
  for (const file of files) {
    const relative = path.relative(root, file).split(path.sep).join("/");
    if (forbiddenUserStatePath(relative)) {
      violations.push(
        `${relative}: user authentication, session, managed-policy, or cache state must not be distributed`,
      );
    }
    const raw = fs.readFileSync(file);
    const body = utf8Text(raw);
    if (body === null) {
      continue;
    }
    if (PRIVATE_KEY_MARKER.test(body)) {
      violations.push(`${relative}: private-key material must not be distributed`);
    }
    const lines = body.split(/\r?\n/);
    const requiresPortablePaths =
      relative === "build-manifest.json" || relative.startsWith("profiles/");
    lines.forEach((line, index) => {
      const lineNumber = index + 1;
      ASSIGNMENT.lastIndex = 0;
      for (const assignment of line.matchAll(ASSIGNMENT)) {
        const key = assignment[2];
        const value =
          assignment[3] ?? assignment[4] ?? assignment[5] ?? "";
        if (isSecretKey(key) && !isPlaceholder(value)) {
          violations.push(
            `${relative}:${lineNumber}: inline secret-like value for ${key}`,
          );
        }
      }
      if (KNOWN_SECRET.test(line)) {
        violations.push(
          `${relative}:${lineNumber}: value resembles a credential or access token`,
        );
      }
      if (
        requiresPortablePaths &&
        !line.startsWith("#!") &&
        (UNIX_MACHINE_PATH.test(line) || WINDOWS_MACHINE_PATH.test(line))
      ) {
        violations.push(
          `${relative}:${lineNumber}: machine-specific absolute path is not portable`,
        );
      }
    });
  }
  return violations;
}

export function assertPayloadPolicy(root) {
  const violations = policyViolations(root);
  if (violations.length > 0) {
    throw new Error(
      `distribution payload policy failed:\n${violations
        .map((item) => `  - ${item}`)
        .join("\n")}`,
    );
  }
}

export { walkFiles };
