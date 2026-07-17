#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { policyViolations } from "./policy.mjs";

function withTempDir(test) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "grok-dist-policy-"));
  try {
    test(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

withTempDir((root) => {
  fs.writeFileSync(
    path.join(root, "config.toml"),
    [
      'env_key = "ANTHROPIC_API_KEY"',
      'base_url = "https://api.example.com/v1"',
      'x-api-key = "${ANTHROPIC_API_KEY}"',
      'authorization = "Bearer $OPENAI_API_KEY"',
      'access_token = "env:GITHUB_TOKEN"',
      'client_secret = "<from-secret-store>"',
      "",
    ].join("\n"),
  );
  assert.deepEqual(policyViolations(root), []);
});

withTempDir((root) => {
  const fakeCredential = ["sk", "ant", "secretsecret"].join("-");
  fs.writeFileSync(
    path.join(root, "config.toml"),
    `api_key = "${fakeCredential}"\n`,
  );
  assert.match(policyViolations(root).join("\n"), /inline secret-like value/);
});

withTempDir((root) => {
  fs.writeFileSync(
    path.join(root, "provider.json.example"),
    '{"ANTHROPIC_API_KEY":"literal-example-secret"}\n',
  );
  assert.match(policyViolations(root).join("\n"), /ANTHROPIC_API_KEY/);
});

withTempDir((root) => {
  fs.writeFileSync(
    path.join(root, "credentials"),
    "client_secret=literal-extensionless-secret\n",
  );
  assert.match(policyViolations(root).join("\n"), /client_secret/);
});

withTempDir((root) => {
  fs.writeFileSync(
    path.join(root, "deploy-key"),
    "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key\n",
  );
  assert.match(policyViolations(root).join("\n"), /private-key material/);
});

for (const relative of [
  ".aws/credentials",
  ".env",
  ".netrc",
  ".npmrc",
  ".ssh/config",
  "auth.json",
  "auth.toml",
  "credentials",
  "sessions/session.jsonl",
  "managed_policy/config.toml",
  "cache/state.db",
]) {
  withTempDir((root) => {
    const file = path.join(root, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, "{}\n");
    assert.match(policyViolations(root).join("\n"), /must not be distributed/);
  });
}

withTempDir((root) => {
  const fakeNpmToken = ["npm", "A".repeat(32)].join("_");
  fs.writeFileSync(
    path.join(root, "npm-config.txt"),
    `//registry.example.test/:_authToken=${fakeNpmToken}\n`,
  );
  assert.match(
    policyViolations(root).join("\n"),
    /credential|inline secret-like value/,
  );
});

withTempDir((root) => {
  fs.mkdirSync(path.join(root, "profiles", "starter", "sessions"), {
    recursive: true,
  });
  assert.match(policyViolations(root).join("\n"), /must not be distributed/);
});

withTempDir((root) => {
  fs.writeFileSync(
    path.join(root, "config.json"),
    '{"headers":{"authorization":"Bearer should-not-ship"}}\n',
  );
  assert.match(policyViolations(root).join("\n"), /inline secret-like value/);
});

for (const [key, value] of [
  ["ANTHROPIC_API_KEY", "literal-anthropic-key"],
  ["x-api-key", "literal-header-key"],
  ["access_token", "literal-access-token"],
  ["client_secret", "literal-client-secret"],
  ["database-password", "literal-password"],
  ["AWS_SECRET_ACCESS_KEY", "literal-aws-key"],
  ["Proxy-Authorization", "literal-proxy-authorization"],
]) {
  withTempDir((root) => {
    fs.writeFileSync(path.join(root, "config.toml"), `${key} = "${value}"\n`);
    assert.match(
      policyViolations(root).join("\n"),
      new RegExp(`inline secret-like value for ${key}`, "i"),
    );
  });
}

withTempDir((root) => {
  fs.writeFileSync(
    path.join(root, "headers.toml"),
    'headers = { "X-Api-Key" = "must-not-ship", "Accept" = "text/plain" }\n',
  );
  assert.match(policyViolations(root).join("\n"), /X-Api-Key/);
});

withTempDir((root) => {
  const profile = path.join(root, "profiles", "starter");
  fs.mkdirSync(profile, { recursive: true });
  fs.writeFileSync(
    path.join(profile, "config.toml"),
    'prompt_file = "/Users/example/private/prompt.md"\n',
  );
  assert.match(policyViolations(root).join("\n"), /absolute path/);
});

withTempDir((root) => {
  fs.writeFileSync(path.join(root, "hook.sh"), "#!/usr/bin/env bash\necho ok\n");
  assert.deepEqual(policyViolations(root), []);
});

process.stdout.write("distribution payload policy tests passed\n");
