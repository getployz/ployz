---
name: controlling-codex
description: Operational manual for supervising Codex CLI runs — launch recipe, event-stream monitoring, drift detection, between-turn steering via resume, and why app-server mid-turn steering is not viable on this machine. Use whenever driving codex exec/app-server as a supervisor per the model-selection skill.
---

# Controlling Codex

Field-tested against codex-cli 0.143.0 on macOS across eight parallel supervised
ticket runs. Verdict up front: **`codex exec --json` + `codex exec resume` is the
default transport.** `codex app-server` is protocol-correct and completed one
full ~200-command run with follow-up turns, but died prematurely in every other
attempt — the causes were environmental, not protocol (sibling supervisors
running `pkill codex`, shared-scratchpad fifo/log clobbering, a model-name
rejection, plus at least one unexplained kill that survived an isolated
`CODEX_HOME`), while sibling `codex exec` processes ran 40+ minutes untouched.
Steering granularity at the turn boundary proved sufficient in practice: Codex
narrates intent in `agent_message` events before acting, so drift is visible
early and a follow-up turn corrects it. Reach for app-server (appendix) only
when a run is long or risky enough that mid-turn steering pays for the
fragility, and only with per-run isolation.

## Launch recipe

```bash
codex exec -C <worktree> -s workspace-write --json \
  -c 'sandbox_workspace_write.network_access=true' \
  -o last-<ticket>.txt - < prompt-<ticket>.md > exec-<ticket>.jsonl 2>&1
```

Run it as a background task (`run_in_background`) — the whole lifecycle must live
in one such task; detached/nohup process groups are reaped between Bash calls.

- Prompt via stdin (`-`) from a file: no shell-quoting pain for multi-KB specs.
- `-o FILE` receives only the final agent message — empty until the turn ends,
  which makes it a cheap completion probe and the first thing to read after.
- Capture the thread id from the first JSONL line
  (`{"type":"thread.started","thread_id":"<uuid>"}`) — it is the resume handle.
- **Ticket-suffix every artifact** (`exec-t310.jsonl`, `prompt-t310.md`).
  Parallel supervisors share the scratchpad and `~/.codex/sessions`; a sibling's
  `>` redirect can replace your event stream mid-run, and `resume --last` can
  resume a sibling's thread. Explicit ids and unique filenames, always.
- Model/effort come from `~/.codex/config.toml`; override with
  `-c model=... -c model_reasoning_effort=...` only when needed.
- Optional hardening: an isolated `CODEX_HOME` (copy `auth.json`, write a 6-line
  config.toml with model/sandbox/approval and the worktree under
  `[projects."<worktree>"] trust_level = "trusted"`) strips fragile user MCP
  servers and notify hooks from the run.

## Reading the event stream

Exec-flavor events are snake_case, one JSON object per line: `thread.started`,
`turn.started`, `item.started`/`item.completed` with `item.type` ∈
`agent_message` (`.text`), `command_execution` (`.command`, `.exit_code`),
`file_change` (`.changes[].path`), `reasoning`, `todo_list`, and terminal
`turn.completed` (with `usage`) or `turn.failed`.

- **The stream is not pure JSONL.** zsh profile noise, `ERROR rmcp::transport`
  lines, and partial tail lines are interleaved. Skip lines not starting `{`,
  wrap `json.loads` in try/except.
- Harmless stderr noise to ignore: `rmcp ... InsufficientScope` (a failing
  user-level MCP server) and `failed to refresh available models: timeout`.
- Poll with a since-cursor renderer that prints only new `agent_message` and
  `command_execution` items. Keep each poll's `sleep` under the 120s Bash
  timeout (`sleep 110`, repeat), or run a `grep -qE '"type":"turn\.(completed|failed)"'`
  until-loop as its own background task so the harness notifies on completion.

**Completion detection needs all three** — any one alone lies: `turn.completed`
in the JSONL, the process gone (`pgrep -f <worktree-path>`, never bare "codex" —
this machine runs a zoo of unrelated codex processes), and `-o`'s file
non-empty. Stream-EOF without `turn.completed` means a crashed turn: check
`git status --short` in the worktree for damage, then resume or relaunch.

## Drift detection

The signal is `agent_message` prose, not file paths: Codex narrates intent
before each edit batch. The trigger pattern is **an intent statement naming a
file outside the ticket's blast radius, justified by the sandbox or environment
rather than the spec** ("in this sandbox, ports can't bind, so I'll change the
test fixture to..."). When it fires, judge severity from `git diff main --
<file>` yourself — narration often nets out to nothing (one run self-reverted
two messages later). The worktree diff is ground truth; the event stream can be
clobbered or lie.

Exec has no mid-turn injection, so the real decision is let-finish vs
kill-and-resume. Let it finish when the damage is confined and revertable; kill
(the worktree freezes safely, the thread persists) and resume with a correction
when it starts building on the wrong path.

## Between-turn steering: resume

Full conversational context survives resume — even a thread started under
app-server resumes into exec seamlessly. Two working forms; the difference is
pure flag placement (exec-level flags must precede the subcommand, otherwise
`error: unexpected argument '-C' found`):

```bash
# flags-before-subcommand — works from anywhere:
codex exec -C <worktree> -s workspace-write --json \
  resume <THREAD_ID> - < fix-<ticket>.txt > exec2-<ticket>.jsonl 2>&1

# flags-after-subcommand — resume runs in *your* cwd, so:
cd <worktree> && codex exec resume <THREAD_ID> \
  -c sandbox_mode="workspace-write" --skip-git-repo-check --json \
  - < fix-<ticket>.txt > exec2-<ticket>.jsonl 2>&1
```

A fresh `codex exec` with a self-contained findings prompt is the better choice
when the first turn's context ballooned (one run hit 9.7M input tokens — a 3KB
findings prompt is cheaper and sharper) or when sibling-session resume ambiguity
is a risk.

Fix-turn prompts that landed in one round: numbered findings, each with
file:line, the quoted broken code, the intended fix shape, and **where the data
lives** (the write-site to extend); plus an explicit do-not-change list so the
fix turn cannot "improve" dismissed judgement calls; ending with the exact
verify commands and "all must be green".

## The first-turn prompt

This is where supervision is won. Spend ten minutes reading the target seams
before writing it. In order:

1. **Process overrides**: "do NOT `git commit` or push — the supervisor
   commits." State it even though the repo /implement skill says to commit;
   Codex notices the conflict and obeys the ticket instruction.
2. **Hard prohibitions and negative scope** as their own block: what
   neighboring tickets own, what must not be built.
3. **Verified orientation map**: "X ALREADY EXISTS at file:symbol — do not
   rebuild it", exact paths for every relevant file, and existing repo patterns
   to copy by name ("follow the `ops watch` polling pattern"). Naming the
   pattern is stronger than naming the rule.
4. **Semantic tripwires as "do NOT X; do Y instead"** — pre-supply the correct
   alternative at every foreseeable wrong turn.
5. **The full spec inline** below a fence — never assume the sandbox reaches
   GitHub or the network.
6. **A demanded final artifact**: the exact verify-command list to run and
   report, files touched, and any concrete claim you intend to check.

## Sandbox realities

- The exec sandbox often cannot bind ports or reach the network: NATS-fixture
  test failures and `ENOTFOUND registry.npmjs.org` are phantom. Warn Codex in
  the prompt to treat harness timeouts as environment, not code — and never let
  it "fix" production or test-support code to dodge its sandbox.
- `danger-full-access` is blocked by the permission classifier;
  `workspace-write` (+ the network-access `-c`) was always sufficient.
- **Verification is the supervisor's job.** Re-run fmt/clippy/tests for touched
  crates yourself after every turn; Codex claiming green is not evidence. Note
  rtk rewrites cargo output — `rtk proxy cargo test` or `tail`, not grep for
  cargo phrasing.

## Appendix: app-server

One run completed a full ~200-command turn plus follow-up turns this way; every
other attempt died to environment. The recipe that worked, and the traps:

- Dump real schemas first: `codex app-server generate-ts --out <dir>` or
  `generate-json-schema --out <dir>` (the latter silently produced 0 bytes in
  one run — verify output exists). `v2/*` is the authoritative param reference.
- **Per-run isolation is mandatory**: unique directory, unique fifo/event
  filenames, absolute paths inside every helper. Sibling supervisors sharing a
  scratchpad clobbered each other's fifos and event logs (one turn's prompt came
  back as a different ticket's), and a sibling's `pkill codex` is the leading
  suspect for the unexplained mid-turn deaths. Check liveness by fifo path
  (`pgrep -f "tail -f <your fifo>"`), never by process name — the box runs a
  zoo of codex processes.
- Two transports worked: (a) the surviving run used
  `nohup sh -c "tail -f <fifo> | codex app-server > <events> 2>&1" &` with each
  message written as a one-shot open/write/close of the fifo (fresh open per
  write; `setsid` doesn't exist on macOS); (b) an owned subprocess
  (`Popen(["codex","app-server","--stdio"], bufsize=0)`) with a stdout-pump
  thread and an outbox-directory poll for stdin. Holding a fifo open across
  Bash calls, or `tail -f` into a pipe with a writer that closes first, wedges
  or EOFs — the two shapes above are the only ones that survived.
- Handshake: `initialize` → `initialized` → `thread/start`
  `{"cwd": <worktree>, "sandbox": "workspace-write", "approvalPolicy": "never"}`
  → `turn/start` `{"threadId", "model": <config model name>, "sandboxPolicy":
  {"type": "workspaceWrite", "networkAccess": true, "writableRoots": [<worktree>]},
  "input": [{"type": "text", "text": ..., "text_elements": []}]}`.
  Thread id at `result.thread.id`; capture `turn/started` → `params.turn.id`
  immediately — `turn/steer` requires it as `expectedTurnId`.
- **Use the config.toml model name in the `model` override.** A `-codex`
  suffixed variant is rejected on ChatGPT accounts (`error` notification with
  "model is not supported", then `turn/completed` `status:"failed"`). A failed
  turn does not kill the thread — the next `turn/start` on the same threadId
  works.
- **Follow-up turns are just `turn/start` on the same threadId** — full context
  survives; a fix turn resolved review findings from file:line hints with no
  re-priming.
- First-token latency is minutes at high reasoning effort — a quiet event file
  is not a stall until process liveness also fails.
- It idle-exits ~15s after `thread/start` if no turn arrives; never park a
  thread. Stdout-EOF without `turn/completed` is the crash signature; threads
  persist in `~/.codex/sessions/`, so recovery is `codex exec resume <id>`.
