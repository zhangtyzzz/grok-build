# Plan Mode

Plan mode is a structured planning phase: the agent explores the codebase and designs an implementation approach before writing any code. Use it for tasks with genuine ambiguity about the right approach, where getting your input before coding prevents significant rework.

---

## What Plan Mode Does

When plan mode is active, the agent:

1. Reads and searches the codebase to understand existing patterns and architecture
2. Designs an implementation approach and writes it to the plan file
3. May use `ask_user_question` to clarify specific questions
4. Calls `exit_plan_mode` to present the plan for your approval

Plan mode is read-only except for the plan file: plan-file edits (`plan.md` in
the session directory) are auto-approved, and edits to any other file are
rejected outright. Commands and tools with unknown or external side effects
(including Bash, write-capable subagents, MCP/meta tools, and generators) are
also rejected fail-closed. This holds in every permission mode, including
always-approve. Purpose-built read, list, search, memory, LSP, and web-fetch
tools remain available.

---

## How to Enter Plan Mode

### Agent-Initiated Entry

The agent enters plan mode when it determines a task has genuine ambiguity. It calls the `enter_plan_mode` tool, which requires your approval before plan mode activates. If you decline, the agent stays in normal mode.

**Good triggers for plan mode:**

- "Add user authentication to the app" -- genuinely ambiguous (session vs JWT, token storage, middleware structure)
- "Redesign the data pipeline" -- major restructuring where the wrong approach wastes significant effort
- "Add caching to the API" -- multiple reasonable approaches (Redis vs in-memory vs file-based)
- "Add real-time updates" -- architectural decision (WebSockets vs SSE vs polling)

**Not appropriate for plan mode:**

- "Add a delete button to the user profile" -- clear implementation path
- "Fix the typo in the README" -- straightforward
- "Update the error handling in the API" -- start working, ask specific questions if needed
- "Can we work on the search feature?" -- user wants to get started, not plan

### User-Initiated Entry

You can enter plan mode yourself in two ways:

- **`/plan`** -- Enter plan mode. Plan mode activates when you send your next prompt. Run `/plan <description>` to enter plan mode and start a turn with that description in one step.
- **Shift+Tab** -- Cycle the session mode: Normal, then Plan, then Always-approve, then back to Normal. From Normal, a single press lands on Plan.

After a plan exists, run **`/view-plan`** (aliases `/show-plan`, `/plan-view`) to reopen its saved preview.

---

## Use a Dedicated Planning Model and Skills

Plan mode can apply a session-scoped model, instructions, and skills without
changing the model selected for other live sessions:

```toml
[provider.anthropic]
base_url = "https://api.anthropic.com/v1"
api_backend = "messages"
auth = "x_api_key"
env_key = "ANTHROPIC_API_KEY"
extra_headers = { "anthropic-version" = "2023-06-01" }
prompt_cache = { mode = "stable_prefix", ttl = "1h" }

[model.claude-planner]
provider = "anthropic"
model = "claude-sonnet"
context_window = 200000

[model_route.planner]
candidates = ["claude-planner"]

[modes.plan]
model = "route:planner"
skills = ["architecture"]
instructions = "Produce an implementation-ready plan with explicit verification."
restore_model = true
```

The mode profile is applied to the existing session; it does not create a
separate planner conversation. Skill bodies and instructions are injected only
into plan turns. On exit, Grok restores the model that was active on entry only
if the session is still using the plan-owned model. A manual `/model` switch
made during planning therefore wins. The model scope is persisted so a resumed
session can safely reconcile an interrupted exit.

Because this is the same conversation, the selected planning provider receives
the session transcript and any read/search results included in the planning
request. A planning model is not a privacy boundary. Configure only providers
that are allowed to receive that project and conversation data.

`model_route` candidates are evaluated in order before a request starts.
Missing models or missing provider credentials move to the next candidate;
Grok never switches provider after a request has begun.

---

## The Plan File

The plan is written to `plan.md` inside the session directory (`~/.grok/sessions/<cwd>/<session-id>/plan.md`, where `<cwd>` is an encoded directory name, not the literal path).

On Unix, the session-owned plan path is accessed through a descriptor-relative
no-follow boundary: a symbolic link, linked parent directory, non-regular file,
or multiply-linked destination is rejected instead of being followed.
Successful writes replace the file atomically. The non-Unix fallback rejects
links seen during validation, but does not yet provide the same guarantee
against a concurrent reparse-point swap; do not treat it as a hostile-writer
security boundary.

The plan file contains:

- A **Context** section explaining why the change is being made
- The recommended approach (not every alternative)
- The paths of critical files to modify
- Existing functions and utilities to reuse, with their file paths
- A verification section describing how to test the changes end to end

---

## Plan Approval

When the agent finishes planning, it calls the `exit_plan_mode` tool. The tool reads the plan file from disk, and the TUI opens a scrollable preview of the plan with an action bar along the bottom.

If the agent exits without writing a plan (empty or missing `plan.md`), the same approval surface still opens with a clear empty-state message so you can approve and start implementing, request changes (send the agent back to planning), or quit. In minimal mode the empty notice is committed into scrollback and the controls strip header reads **No plan written yet**.

### Reviewing the Plan

Scroll the plan with the arrow keys or `j`/`k`. The action bar shows these shortcuts:

| Shortcut | Action                                                                                               |
| -------- | ---------------------------------------------------------------------------------------------------- |
| `a`      | Approve the plan and start building. With pending comments, this reads `approve w/ comments` and sends them alongside the approval. |
| `s`      | Request changes. Focus moves to the prompt so you can type revision notes; press `Enter` to send them. |
| `c`      | Comment on the selected line or line range.                                                          |
| `q`      | Quit plan -- abandon the plan without approving and turn plan mode off.                              |

Press `Tab` to move focus between the plan preview and the prompt.

### Providing Feedback

The approval view has three focus states:

- **Preview**: Scroll the plan and select lines to comment on.
- **Commenting**: Add an inline comment to the selected line range (press `c`, or `Enter` on a line).
- **Prompt**: Type freeform revision notes.

Press `Tab` to switch between the preview and the prompt. When you send feedback -- inline comments, freeform notes, or both -- the agent receives it and revises the plan. Plan mode stays active so you can iterate.

### Leaving the Approval View

Press `Esc` to return focus from the prompt to the plan preview. To dismiss the approval without approving or sending feedback, press `q` to quit the plan. Quitting abandons the proposed plan and turns plan mode off.

---

## Plan Mode Lifecycle

The plan mode state machine has four states:

| State          | Description                                                    |
| -------------- | -------------------------------------------------------------- |
| `Inactive`     | Normal operating mode. No plan mode constraints.               |
| `Pending`      | Client toggled plan mode ON, but no prompt has been sent yet.  |
| `Active`       | Plan mode is active. Plan-file edits are auto-approved; edits to other files are rejected. |
| `ExitPending`  | User toggled plan mode OFF while a turn is in-flight.          |

Transitions:

```
Inactive    --> Active   (enter_plan_mode tool called and approved -- skips Pending)
Inactive    --> Pending  (you toggle plan mode on with /plan or Shift+Tab)
Pending     --> Active   (your first prompt activates plan mode)
Active      --> Inactive (exit_plan_mode approved, or you toggle plan mode off when idle)
Active      --> ExitPending (you toggle plan mode off while a turn is in-flight)
ExitPending --> Inactive (after the turn completes)
```

Plan mode state is persisted to disk and survives process restarts. Transient states (`Pending`, `ExitPending`) are collapsed to `Inactive` on restart since they depend on in-flight interactions.

---

## Edits During Plan Mode

During active plan mode, edits to the plan file are auto-approved without prompting, so the agent can iterate on the plan freely. Edits to **any other file are rejected** before they run — the agent receives a short message naming the plan file as the only editable path.

This enforcement is independent of the permission mode:

- **Always-approve (yolo) stays armed underneath plan mode**, but it cannot
  bypass the plan gate. Once the plan is approved, always-approve resumes for
  implementation.
- Bash and monitor commands are rejected rather than heuristically classified;
  shell redirection therefore cannot write around the gate.
- Starting a subagent is rejected while plan mode is active, because a child
  could have a broader toolset than the parent.
- MCP, dynamic/meta-dispatch, scheduler mutation, and media-generation tools
  are rejected because their side effects cannot be proven locally.
- Purpose-built read/search tools and the configured plan skills stay
  available. The plan file remains the sole writable path.

The status flag shows `plan` while plan mode is active. If always-approve is enabled underneath, its flag reappears when plan mode exits.

---

## Plan Mode and Compaction

When `/compact` runs during an active plan mode session, the plan mode state is preserved. The compacted context includes a reminder that plan mode is active, so the agent continues planning after compaction.

---

## When Plan Mode is Appropriate

**Use plan mode for:**

- Tasks with significant architectural ambiguity (multiple reasonable approaches)
- Unclear requirements that need exploration before implementation
- High-impact restructuring where the wrong approach wastes significant effort

**Skip plan mode for:**

- Tasks with a clear implementation path
- Bug fixes where the fix is obvious once you understand the bug
- Adding features that follow existing conventions
- Straightforward modifications (renaming, formatting, adding tests)
- Research and exploration tasks (use subagents instead)
