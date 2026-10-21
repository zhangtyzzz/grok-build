# Live-model Code Mode check (2026-10-21)

This small check supports Code Mode for selected read/filter workloads. It does
not establish a general coding improvement or justify enabling it by default.

The actual CLI and a configured `qwen3.8-flash` model ran 24 fresh sessions: three
synthetic tasks, two tool profiles, two repeats and two modes. All 24 returned the
independently calculated exact JSON answer. Prompts did not request Code Mode.
Repeat 1 ran direct then opt-in; repeat 2 reversed that order. Each session had a
fresh workspace and configuration home, memory and auxiliary summary/title calls
disabled, a 12-turn limit and a 240-second timeout. No failed runs were discarded.

## Results

Numbers below are the mean of two runs per cell; raw per-run results, usage and
actual adoption are in [the measured data](evidence/code-mode-live-2026-10-21.json).
Total tokens include uncached input, cached input and output. Reasoning is an
output subset and is not added again. These totals include advertised schemas,
programs, catalog interactions and outer envelopes.

| Tool profile / task | Direct model rounds | Opt-in model rounds | Direct total tokens | Opt-in total tokens | Sessions using execute |
| --- | ---: | ---: | ---: | ---: | ---: |
| Read-only / dependent lookup | 9.5 | 8 | 99,285.5 | 88,832.5 | 1/2 |
| Read-only / four-file counting | 7.5 | 4.5 | 96,286 | 38,103 | 2/2 |
| Read-only / tiny lookup | 3 | 2.5 | 19,072.5 | 16,903.5 | 0/2 |
| Terminal allowed / dependent lookup | 7.5 | 6 | 121,579 | 88,280 | 0/2 |
| Terminal allowed / four-file counting | 4.5 | 5 | 66,144 | 77,646.5 | 0/2 |
| Terminal allowed / tiny lookup | 3.5 | 3 | 48,060 | 41,780.5 | 0/2 |

Four-file counting with read-only tools reduced mean model rounds by 40% and
total tokens by 60.4%, with both repeats correct and actually using execute.
One model-written program used `Promise.allSettled` to read four files and return
only status tallies. The five programs across these two sessions made 20 actual
leaf calls; tool work was not replaced by a fabricated single-call count.

The other execute invocation only searched the catalog. Most opt-in sessions
used direct tools. Differences without script use cannot be attributed to the
composition mechanism. With terminal access, both modes used ordinary terminal
commands; no opt-in session used execute, and four-file counting cost 17.4% more
mean tokens in the opt-in group. Code Mode is an additional affordance, and its
schema/choice overhead can outweigh its value when the model already writes a
short terminal script.

All six execute programs finished successfully and all 20 admitted leaf calls
had results. All recorded ordinary call IDs were nonempty after the streaming-ID
fix. This checks successful scripts; cancellation and partial-write failures
are covered by the separate hermetic real-worker tests.

## Fixtures and reproduction

Generate inputs outside the repository with
`scripts/bench-code-mode-fixtures.py DIRECTORY TASK`. The script prints the exact
neutral prompt and operator-only expected value; it writes only input files.
Tasks are:

- `dependent`: index -> active release -> selected IDs across four 100-record
  shards, then sum selected amounts.
- `parallel`: four 90-record service files, count exact `error` status per file.
- `tiny`: read one port from a three-field JSON file.

For each fresh directory and provider-only configuration home, run the same
prompt and model twice with `GROK_CODE_MODE=0` and `GROK_CODE_MODE=1`. Use
`--tools read_file,list_dir,grep` for the read-only profile and append
`,run_terminal_command` for the terminal profile. The native dispatch companion
tools remain available. Actual read-only calls were read_file/list_dir/grep/
execute; terminal-profile calls were read_file/run_terminal_command. Use
`--output-format json --no-auto-update --max-turns 12`, the same permission policy
for both modes, and the same 240-second external timeout. Repeat in reverse order.
Retain stdout, session history and code-mode traces; score the actual final JSON
against the generator's value outside the model workspace.

This is two repetitions of three small synthetic tasks with one model, not a
business coding benchmark or a significance test. Other local workloads were
active; wall time is retained as context and is not a controlled latency claim.
The source commit and exact tested binary SHA256 are in the data. Its `--version`
stamp was stale after a shared-target build and still displayed 29f6fdef; that
stamp is not evidence of the tested source. The build log, clean ea9ae0e source,
actual execute advertisement/traces and same-binary paired comparison establish
what ran. Do not describe these runs as the stock stable release.
