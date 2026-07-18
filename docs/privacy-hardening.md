# Privacy hardening

Custom release artifacts are built with the `release-dist` Cargo feature. That
feature enables a compile-time privacy boundary in both `xai-grok-shell` and
`xai-grok-telemetry`; it is not a preference that environment variables,
managed configuration, or remote settings can turn back on.

## Blocked in distribution artifacts

- product analytics and Mixpanel events, including session-metrics mode;
- the first-party internal OTLP trace exporter;
- user-configured external OTEL log and metrics exporters;
- trace/session artifact upload gates and heap-profile upload;
- Sentry error reporting;
- feedback requests/submissions and inline review-comment cloud events;
- remote session registry updates; and
- session storage writeback and search-index remote sync. Sessions remain
  local.

The shell closes the runtime gates, and the telemetry crate independently
refuses to create product, Sentry, internal OTLP, or external OTEL clients.
This defense in depth keeps those channels dormant even if a future call site
passes enabled configuration by mistake.

## Intentionally retained network behavior

Privacy hardening does not make Grok Build an offline application. These
product functions still require network access:

- authentication and subscription checks;
- model inference, including the prompt, selected context, tool results, and
  files intentionally supplied to the configured model provider;
- remote settings and bundled product resources needed by the runtime;
- the fork's GitHub release update checks and downloads; and
- explicit session-sharing, network tools, MCP servers, media services, or
  provider endpoints invoked by the user or agent.

Use an operating-system firewall or an allowlist proxy when a stricter
destination boundary is required.

## Verify an artifact

Run:

```sh
grok inspect --json
```

A custom distribution must report:

```json
{
  "privacyHardened": true
}
```

The GitHub release workflow checks this field on every native macOS, Linux,
and Windows artifact before publication. A normal developer build without
`--features release-dist` reports `false` and retains upstream behavior.
