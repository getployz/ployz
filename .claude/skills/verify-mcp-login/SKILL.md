---
name: verify-mcp-login
description: Model-check `ployz mcp login`, which mints the agent token `ployz mcp` acts with and keeps it in mcp.json beside the config, against a second login stepping between any two of its reads, Cloud calls and writes, Cloud answering a token of kind cli, a save that fails before or after its rename, and a list or DELETE that fails, from six start files, with the Stateright model in core/crates/ployz-verify/src/mcp_login.rs. Use when a change touches core/crates/ployz/src/handlers/mcp/login.rs (login, stored, save, Saved, fate, revoke, warnings, Left, Stored, path, LIFETIME_DAYS), core/crates/ployz-decide/src/fate.rs (fate, Previous, Durability, Why, Fate), the callers of login::stored in core/crates/ployz/src/handlers/mcp.rs (publishing_setup, agent), in_cloud in core/crates/ployz/src/handlers/account.rs, new_token, credentials or remove_token in core/crates/ployz/src/cloud_account/token.rs, or Cloud's /api/cli/tokens routes (POST tokens, GET tokens, DELETE tokens/:id and pendingRevocation in dashboard/src/routes/api/cli/-cli.handler.ts; createOrganizationToken, mintOrganizationTokenUnchecked, listCredentials and revokeCredential in dashboard/src/modules/identity/organization-token.server.ts).
---

# Verify the MCP login token

The model in `core/crates/ployz-verify/src/mcp_login.rs` explores every interleaving of two `ployz mcp login` processes that read and write `mcp.json` and the Organization Tokens of kind `agent` it names (`dashboard/CONTEXT.md:100-102`). A bare `:n` below is a line of the file named just before it in the same parentheses, and otherwise of `core/crates/ployz/src/handlers/mcp/login.rs`. Every path is written in full from the repo root. Every command runs from the repo root.

## What the model copies

**The processes.** Each login is its own `ployz mcp login` process. `login` reads `mcp.json` (`:92`) and reports (`:160-178`) on its own thread. Its Cloud calls and its `save` (`:101-158`) run inside `in_cloud`, whose `block_on` holds that thread (`core/crates/ployz/src/handlers/account.rs:133-142`) on a current-thread runtime (`runtime`, `core/crates/ployz/src/handlers/mod.rs:472-476`). So one login's steps run in order, and the other login can step between any two of them. The model runs two, Login A and Login B (`MAX_LOGINS`).

**One login, step by step.** Each row is one model step (`Pc`), and the step does exactly what its lines do.

| Step | Lines | What it touches others can see |
|---|---|---|
| `Idle`: `stored` reads `mcp.json`; a `Corrupt` file warns "… is unreadable. Replacing it." and goes on with none | `:92-99`, `:48-57` | `mcp.json`, read |
| `Minting`: `in_cloud`'s credential load, then `new_token`'s `POST /api/cli/tokens` | `core/crates/ployz/src/handlers/account.rs:139`, `:101-108` | a new token row |
| `Saving`: the kind check, `save` (temp file, `sync_all`, rename, directory flush), then `fate` | `:109-124`, `:66-88`, `:140-141`, `ployz_decide::fate::fate` | `mcp.json`, written by the rename |
| `Unmaking`: `remove_token` of the token just made, and the failed-save warning if it fails | `:125-137` | that token's row |
| `Listing`: `revoke`'s `GET /api/cli/tokens` and its listed-as-agent check | `:255-262` | token rows, read |
| `Deleting`: `revoke`'s `DELETE /api/cli/tokens/<id>` | `:263` | the token before's row |
| `Reporting`: the warnings, the note and "Revoked …" | `:160-178`, `:181-199` | nothing |

**Why each step is one step.**
- **`mcp.json` is one file beside the config** (`path`, `:40-42`). A read is one `std::fs::read` (`:50`), and a replace is one rename (`file.persist`, `:79`). So a reader sees the old file or the new one.
- **Cloud is the CLI's `/api/cli/tokens` routes** (`dashboard/src/routes/api/cli/-cli.handler.ts:202-233`). `POST` inserts one row (`mintOrganizationTokenUnchecked`, `dashboard/src/modules/identity/organization-token.server.ts:34-56`). `GET` reads the rows (`listCredentials`, `dashboard/src/modules/identity/organization-token.server.ts:119-168`). `DELETE` is one `DELETE … RETURNING` (`dashboard/src/modules/identity/organization-token.server.ts:174-177`), and when it deletes no row, `pendingRevocation` answers (`dashboard/src/routes/api/cli/-cli.handler.ts:229-230`, `:572-577`). Each call's statement is atomic, and the CLI acts only on the reply, so each call is one step.
- **Merged steps touch only their own process.** `Saving` joins the kind check, the temp file's write and `sync_all`, the rename, the directory flush and `fate`. No other process reads a temp file, the directory flush changes only what survives a crash (which no step models), and `fate` reads only the token read and the `Saved` answer. So only the rename is seen, and it is atomic. `Minting` joins the credential load, which reads `cloud.json`, a file no modelled step writes (`cloud.json` is `.claude/skills/verify-sign-in-store/SKILL.md`'s model), with the `POST`.

**The reads, audited against the other login's steps.**
- **`stored` at `:92` against the other's rename (`:79`).** The rename can land just before the read or just after it, and between the read and this login's own rename. The read and the rename are separate steps, so the model reaches each order. The rename reads nothing (`:79`), so the model's rename reads nothing: it never compares `mcp.json` with what its login read.
- **The list at `:255` against the other's `DELETE`.** The `DELETE` can land before the list, so the list misses the token (`Ok(false)`, `:147`), or between the list and this login's `DELETE` (a 404). Both are separate steps, and both orders are reached (two `Reach::AsIs` rows).
- **The `DELETE` at `:263` against the other's `DELETE`.** Each is one statement. The second finds no row and gets a 404.

**Every writer and reader of `mcp.json`.** This grep lists them:

```
git grep -n -E 'login::stored|stored\(&config|^\s+save\($' -- core/crates/ployz/src
```

At the sync commit it prints four lines:

```
core/crates/ployz/src/handlers/mcp.rs:58:    match login::stored(config) {
core/crates/ployz/src/handlers/mcp.rs:689:        match login::stored(&self.account.config) {
core/crates/ployz/src/handlers/mcp/login.rs:92:    let previous = match stored(&config) {
core/crates/ployz/src/handlers/mcp/login.rs:110:            save(
```

`save` is the one writer, and `login`'s call at `:110` is its one call. `publishing_setup` (`core/crates/ployz/src/handlers/mcp.rs:54-64`) and `agent` (`core/crates/ployz/src/handlers/mcp.rs:680-694`) read it and write nothing, and `agent` reads it on each call (`core/crates/ployz/src/handlers/mcp.rs:689`). So readers are left out: they change no state a login reads. A hand edit of `mcp.json` is left out too, except as a start file.

**Every writer of a token row.** This grep lists them:

```
git grep -n -E 'delete\(organizationToken\)|insert\(organizationToken\)|update\(organizationToken\)' -- dashboard/src ':!*.test.ts'
```

At the sync commit it prints four lines:

```
dashboard/src/modules/identity/claim.server.ts:131:    yield* drizzle.update(organizationToken)
dashboard/src/modules/identity/organization-token.server.ts:46:    .insert(organizationToken)
dashboard/src/modules/identity/organization-token.server.ts:175:    .delete(organizationToken)
dashboard/src/modules/organization/membership.server.ts:44:    yield* drizzle.delete(organizationToken)
```

The model copies the insert (through the CLI's `POST`) and the `DELETE … RETURNING` (through the CLI's `DELETE`). It leaves out the other callers of that `DELETE` and the other two writers: the dashboard's revoke (`dashboard/src/modules/identity/organization-token.functions.ts:38-44`), `ployz token rm` (`core/crates/ployz/src/handlers/account.rs:243-268`), a member's removal (`dashboard/src/modules/organization/membership.server.ts:44-45`), and a sandbox claim's `update` (`dashboard/src/modules/identity/claim.server.ts:131-133`).

**What Cloud answers.** This checkout's Cloud sets a new token's kind to `caller.via === "agent" ? "agent" : (input.kind ?? "cli")` (`dashboard/src/modules/identity/organization-token.server.ts:70`), so it answers kind `agent` whenever the CLI asks for it, as `ployz mcp login` does (`:104`). The model still lets Cloud answer kind `cli` once (`CLI_ANSWERS`), because the kind check (`:109`, `:119-124`) guards against a Cloud that ignores `kind`. It claims nothing about which Cloud releases do that.

**The start files** (`Start`, `STARTS`). Each needs a row, and dropping it turns tests red (K19 to K24):
- no `mcp.json`;
- `P`, an agent token in this Cloud and Organization;
- `P` in another Organization of this Cloud;
- `P` in another Cloud, under the same Organization slug. With the Cloud check removed, only this start gets past the Organization check to `revoke`;
- an edited file naming `P`, an Organization Token of kind `cli` in this Organization;
- an unreadable file, whose token nobody can know.

**Failures, each a one-off** (`How::Fails`, `How::Unflushed`, and the bounds). A save fails before its rename (`SAVE_FAILURES`). A save renames, then its directory flush fails (`FLUSH_FAILURES`). `revoke`'s list fails (`LIST_FAILURES`). A `DELETE` fails, `revoke`'s or the one of a token just made (`DELETE_FAILURES`, shared). A failed Cloud call is "Could not reach Cloud", which leaves the row as it was.

A lasting failure differs, as the code shows. If every list or every `DELETE` fails, each login that reaches `revoke` leaves its token before live and names it in a `Left` warning (`:148-154`). The next login's token before is then the last login's own token, not the earlier ones, so the earlier ones stay live with only their old warning naming them. If every directory flush fails, each login leaves its token before live for the Unflushed arm (`:229-235`) and warns twice (`:185-197`).

**The properties.** All five are `always`.

| Const | String a failure prints |
|---|---|
| `EVERY_TOKEN_ACCOUNTED` | "once every login has returned, every agent token a login made, and the agent token mcp.json held at the start, is the one mcp.json holds, revoked, or named by id in a warning that names a token left unrevoked" |
| `NOTE_HOLDS_WHEN_PRINTED` | "when a login prints that \`ployz mcp\` now acts with its token, mcp.json holds that token" |
| `REVOKED_ONLY_WHEN_DURABLE` | "a login revokes the token it replaced only after its own save flushed the temp file before the rename, and the directory after it" |
| `ONLY_AGENT_TOKEN_HELD` | "no login's rename puts a token whose kind is not agent into mcp.json" |
| `NO_CLI_TOKEN_REVOKED` | "no login revokes an Organization Token of kind cli as the token it replaced" |

A `COVERAGE` row prints its `name`.

What grounds each:
- **`EVERY_TOKEN_ACCOUNTED`** comes from `docs/user/cli/overview.md:116`: "Running `ployz mcp login` again makes a new one and revokes the old." It watches every agent token: `P`, and each one a login made (`counts`), not only the last. "Revoked" is a row gone from Cloud. Only two warnings name a token: the `Left` warning, "The agent token {} this replaced is still live: {}. To revoke it, {}." (`:192-197`), and the failed-save warning, "Could not revoke the token {} just made: {revoking}" then "Run \`ployz token rm {}\`." (`:129-135`). The Unflushed warning names a path, not a token (`:185-191`), so it doesn't count. A token of kind `cli` is not an agent token, and the property doesn't count it. `accounted` and `counts` are the predicate, and `a_token_is_accounted_only_when_held_revoked_or_named` pins each edge.
- **`NOTE_HOLDS_WHEN_PRINTED`** comes from the note, "\`ployz mcp\` now acts with its own agent token {} in Organization {}, expiring {}." (`:172`), and from `docs/user/cli/overview.md:364-365`, where `ployz token ls` lists "`agent` for the one `ployz mcp login` made". `save` is the one writer, and a login prints its note only after its own rename, so it breaks exactly when another login's rename lands between this login's rename (`:79`) and its note (`:171-174`). It reads the note from `State::took`, so it adds no state. `--json` prints no note (`:163-170`), and the model follows the human output.
- **`REVOKED_ONLY_WHEN_DURABLE`** comes from the comment "Durable before the caller revokes the token this replaces." (`:75`) and `fate`'s doc comment "The agent token \`mcp.json\` held before is revoked with this login's sign-in only in the Cloud and Organization that sign-in acts in, and only once the new token is durable." (`:209-210`). It reads `Login::flushes`, the facts of what the login's save did, not `Saved`, the code's answer, so it doesn't read the guard it tests. The durability bookkeeping follows the code's own comments (`:59-64`, `:75`, `:81`, `:209-210`); no step models a power loss, and no lane checks one.
- **`ONLY_AGENT_TOKEN_HELD`** comes from `login_command`'s about, "Give \`ployz mcp\` its own agent token, which answers no approval" (`:19`), and `docs/user/cli/overview.md:118`, "An agent token can't answer an approval, so the agent can't approve its own calls through \`ployz mcp\`." It speaks of a rename, not of what `mcp.json` holds, because the edited start file holds a `cli` token before any login runs.
- **`NO_CLI_TOKEN_REVOKED`** comes from `revoke`'s doc comment, "Revoke the token \`ployz mcp\` held before, only while this Organization lists it as an agent token, so an edited \`mcp.json\` never revokes a CLI token." (`:252-253`). A login's revoke of the token it just made (`:128`) is not covered.

**Bookkeeping no step reads.** `State::renames`, `Login::saw` and `State::raced` keep the race in the state for the negative searches. `renames` counts the renames so far, `saw` is that count at a login's read, and `raced` turns on, and stays on, when a login renames after another login's rename that landed after its own read. None of them is a step of the code: the rename at `:79` reads nothing, and `raced` is not a compare it makes. `breaks_where` sees only the first path to each state, so a filter must read a fact the state holds, such as `raced`, not one only the path shows.

## The finding

Two logins read the same token before, and both mint. The first rename's token is replaced by the second's. It stays live in Cloud, and no login revokes it or names it. Both return `Ok`, and its login prints that `ployz mcp` now acts with it.

`two_logins_that_read_the_same_token_orphan_the_first_rename` replays it from `P` here:
1. A reads `P` (`:92`), and B reads `P` (`:92`);
2. A mints TA, and B mints TB (`:101-108`);
3. A's save renames TA over `P`, and B's renames TB over TA, each without reading the file again (`:79`);
4. A lists `P` as an agent token and deletes it (`:255-263`). B lists after that `DELETE`, finds no `P`, and says nothing (`:147`);
5. both return `Ok`, and each prints its note (`:171-174`).

`mcp.json` holds TB. TA is live in Cloud, and no login revoked it or named it. A's note said `ployz mcp` now acts with TA while `mcp.json` held TB. Both properties break: `EVERY_TOKEN_ACCOUNTED` and `NOTE_HOLDS_WHEN_PRINTED`.

How far this departs from the docs (`docs/user/cli/overview.md:116`): a login that runs alone, whose token before is an agent token in this Cloud and Organization, and whose save, list and `DELETE` all succeed, makes a new token and revokes the old one. Otherwise a lone login leaves the old token live: `fate` leaves one in another Cloud or Organization, or after an Unflushed save (`:213-235`), a failed list or `DELETE` leaves it (`:148-154`), and each of these names it in a warning; `revoke` leaves a `cli` token and says nothing (`:256-261`). The model reaches each of these with one login. The finding is about the new token of an overlapping login, which no warning names.

**Its timing condition.** B's read (`:92`) lands before A's rename (`:79`), and B's rename lands after A's. The login whose rename lands first loses its token, whichever finished first. A login's window from its read to its rename holds `in_cloud`'s credential load (`core/crates/ployz/src/handlers/account.rs:139`), `new_token`'s `POST` (`:101-108`), and the temp file's write and `sync_all` (`:71-78`). The model has no clock: it shows the schedules that break, not how often they happen.

**The first-login route.** With no `mcp.json`, both logins read none, both mint, and A then B renames. No revoke runs, and TA is live and unnamed (`two_first_logins_orphan_the_first_rename`).

**The second route to a stale note, which orphans no token.** B reads after A's rename, so it reads TA as its token before, and B's rename lands before A's note. B then revokes TA, so every token is accounted; only A's note was false when printed (`a_login_that_reads_the_other_s_token_makes_its_note_stale`). Keep it apart from the race: its condition is B's read after A's rename and B's rename before A's note.

**The revoke-side misreport.** When both logins read `P`, the second to act on `P` misreports it, in one of two ways, by its condition:
- its list lands after the first `DELETE`: it finds no `P` and says nothing (`Ok(false)`, `:147`). The finding's replay takes this way;
- its list lands before the first `DELETE`, and its `DELETE` after: Cloud has no row, and with no Server Clear pending for `P`, `pendingRevocation` re-raises the `NotFound` (`dashboard/src/routes/api/cli/-cli.handler.ts:572-577`). The CLI reads the 404 as `UnknownCredential` (`remove_token` in `core/crates/ployz/src/cloud_account/token.rs`), and the login warns that `P` "is still live" though its row is gone (`:148-154`).

Both are `Reach::AsIs` rows, and no property covers them: `P` is revoked either way.

**Nothing a later login does ends the orphan.** `fate`'s id is the token that login read (`:212`), and the orphan is in no file. Things that do end it: `ployz token rm` (`core/crates/ployz/src/handlers/account.rs:243-268`), the dashboard's revoke (`dashboard/src/modules/identity/organization-token.functions.ts:38-44`), the member leaving the Organization (`dashboard/src/modules/organization/membership.server.ts:44-45`), and expiry after `LIFETIME_DAYS` (`:13`, `:106`; `dashboard/src/modules/identity/organization-token.server.ts:71`), after which Cloud refuses the token as a bearer (`tokenRuns`, `dashboard/src/modules/identity/caller.server.ts:129-130`, `dashboard/src/modules/identity/claim-required.server.ts:40`), though `ployz token ls` still lists its row as `expired` (`dashboard/src/modules/identity/organization-token.server.ts:157`). `AgentTokenRefused`'s text also names the Organization's deletion (`core/crates/ployz/src/cloud_login.rs:80`). This list may not be complete.

**Only a race orphans a token.** `only_a_race_orphans_a_token` searches the whole model with every raced state rejected and finds no break of `EVERY_TOKEN_ACCOUNTED`. `the_unraced_stale_note_is_the_second_route` runs the same search for `NOTE_HOLDS_WHEN_PRINTED`, and the break it finds is the second route.

## Run it

```
cd core && cargo nextest run -p ployz-verify mcp_login --no-capture
```

Expected: `24 tests run: 24 passed`. `current_code_breaks_only_the_finding` prints `as is: 4104 states`, and `the_fix_holds` prints `fixed: 1114 states`. Each story prints under its test's name, and the stories are the same from run to run. CI runs `ployz-verify` in its workspace nextest run.

The `cargo test` form prints the same stories:

```
cd core && cargo test -p ployz-verify mcp_login -- --nocapture
```

Expected: `test result: ok. 24 passed; 0 failed`.

**Run times.** Count compiling and running apart; both grow with host load.
- **Running.** The `mcp_login` tests take 0.1 to 1 s of nextest time. The logged runs took 0.27 to 0.56 s.
- **Without an edit.** A warm run, with nothing to compile, takes 0.5 to 4 s of wall time. The logged runs took 0.97 to 2.64 s.
- **After an edit to the model.** Cargo recompiles `ployz-verify` first. Across the logged scratch runs, on hosts with load averages up to about 26, an edit-and-run took 1.9 to 12.3 s of wall time, the compile alone up to 10 s; allow up to 20 s.
- **Cold.** A dev build in a fresh `CARGO_TARGET_DIR` took 72 s of wall time. A release build (`--release`, after `rm -rf core/target/release`) took 8.8 s here, with the dependencies already built; no release build from an empty target was logged. The dev profile builds `ployz-verify` and `stateright` at `opt-level = 3` already (`core/Cargo.toml:36-40`).

Read the tests in five groups.
- **The finding, on the code as it is.**
  - `two_logins_that_read_the_same_token_orphan_the_first_rename` and `two_first_logins_orphan_the_first_rename` replay each route with `follow`, assert each step's branch and the end state.
  - `the_first_orphan_is_a_race` asks the checker for the first break of `EVERY_TOKEN_ACCOUNTED` and asserts it is a race: one login reads before the other's rename, and renames after it.
  - `a_login_that_reads_the_other_s_token_makes_its_note_stale` replays the second route, and asserts every token is accounted at its end.
  - `the_first_stale_note_follows_another_login_s_rename` asserts only that another login's rename lands between the printing login's rename and its note. BFS may return either route, so it doesn't assert which.
  - `only_a_race_orphans_a_token` and `the_unraced_stale_note_is_the_second_route` are the unraced searches above.
  - `current_code_breaks_only_the_finding` expects `EVERY_TOKEN_ACCOUNTED` and `NOTE_HOLDS_WHEN_PRINTED` to break, the other three to hold, and every row that is not `Reach::Fix` to be reached.
- **The code's guards.** `the_temp_file_is_flushed_before_the_rename`, `an_unflushed_save_keeps_the_token_before`, `fate_leaves_a_token_in_another_cloud`, `fate_leaves_a_token_in_another_organization`, `revoke_deletes_only_a_listed_agent_token`, `the_kind_check_refuses_a_cli_token` and `a_failed_save_revokes_the_token_just_made` each set one flag on `as_is_without(..)` (Breaking a guard yourself). The three that break `EVERY_TOKEN_ACCOUNTED` search with every raced state rejected, so the flag, not the race, is what breaks it.
- **The fix.** `the_fix_holds` runs `fixed()` with `assert_properties`: all five `always` hold, and every row that is not `Reach::AsIs` is reached. `the_fix_serializes_the_finding_s_logins`, `the_fix_holds_off_a_second_login`, `the_fix_releases_the_lock_when_a_login_fails`, `the_fix_revokes_a_first_login_s_token` and `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named` check its edge states (The fix).
- **The fix's parts.** `the_fix_needs_the_lock_from_the_read` and `the_fix_needs_the_lock_through_the_report` each set one flag on `fixed_without(..)`.
- **The predicate.** `a_token_is_accounted_only_when_held_revoked_or_named` checks `accounted` and `counts` at their edges.

## The fix

The fix variant models E-142's lock. No product code has it yet.

```text
login     take an exclusive lock on a file beside mcp.json, in the shape of
          Config::update (core/crates/ployz/src/context.rs:299-326);
          read mcp.json, mint, save, fate, revoke, warn and note as today;
          release the lock as the login returns, whichever way
```

So a second login waits until the first returns, reads the first one's token as its token before, and revokes it as `fate` and `revoke` do today. Readers (`agent`, `publishing_setup`) take no lock, since a rename is atomic. Only `ployz mcp login` takes this lock.

In the model, `McpLogin::locks` turns it on. `McpLogin::read` takes the lock and reads in one step: every writer takes the lock first, so nothing can write between them. `McpLogin::offers` offers no read while another login holds it. `McpLogin::step` releases it when its login returns.

**Each part, and what keeps it.**

| Part | Flag | What breaks without it | Test |
|---|---|---|---|
| The lock from the read | `fix_unlocked` | `EVERY_TOKEN_ACCOUNTED`, through the race | `the_fix_needs_the_lock_from_the_read` |
| Holding it through the report | `fix_releases_before_report`, which releases after the rename | `NOTE_HOLDS_WHEN_PRINTED`, through the second route; `EVERY_TOKEN_ACCOUNTED` holds | `the_fix_needs_the_lock_through_the_report` |
| Releasing it as the login returns | none: K12 deletes the release in `McpLogin::step` | no `always`. The other login waits forever, and only two rows go unreached: the `Reach::Fix` row and "every login returns Ok" (176 states) | `the_fix_holds`, through its rows |

The model has no liveness property, so a lock that is never released shows only as unreached `COVERAGE` rows. An early release (the flag) and no release (K12) are different breaks.

**Its edge states.**
- the lock is free, and the first login to read takes it; while it holds it, the other login is offered no step (`the_fix_holds_off_a_second_login`);
- the holder's save fails, or its kind check refuses, and it releases the lock as it returns; the next login reads the unchanged `P` (`the_fix_releases_the_lock_when_a_login_fails`);
- the holder was a first login with no `mcp.json`, and the next login reads its token and revokes it (`the_fix_revokes_a_first_login_s_token`);
- the holder left `P` live (Unflushed, or `P` in another Organization or Cloud), and the next login reads the holder's token and revokes it, while `P` stays named (`the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named`);
- the finding's logins: B is offered no read while A holds the lock, then reads TA and revokes it, and every `always` holds along the way (`the_fix_serializes_the_finding_s_logins`).

**E-142 also names a re-read before persist.** With every writer taking the lock, that re-read never sees another token in this model: no modelled step writes `mcp.json` but a login's `save` (`:110`), and a hand edit during a login is not modelled. It matters only against a writer that takes no lock, an older `ployz` or a hand edit. So the model leaves the re-read out, and E-142 must argue it.

**What it costs.** A second login waits on the first's Cloud calls, each up to `REQUEST_TIMEOUT`, 15 s (`core/crates/ployz/src/cloud_account/http.rs`).

**What it doesn't change.** A kill, a power loss or a lost reply still leaves a token unnamed (Bounds). After an Unflushed save, a crash may bring the token before back into `mcp.json` (`:62`); the new token is then live and held by no file, and the Unflushed warning names `mcp.json`'s path, not that token's id (`:185-191`).

## Read a counterexample

A story is a numbered list of steps cut at the last state of a path. Each row names who takes the step, Login A or Login B, then "[fails]" or "[unflushed]" when the step took that choice, then the statements it copies with their lines. The line under each row is the state after the step: `mcp.json`, the lock under the fix, Cloud's tokens with their kind, place and whether each is live or revoked, each login's next step and the token it read, and the tokens a warning named. `P` is the start file's token, and TA and TB are Login A's and Login B's. A row that skips a guard says "(under Broken)" when a flag is set. A story that starts mid-path starts with "from the state above". `follow` prints "a schedule" where `breaks` prints the property.

These are the last rows of `two_logins_that_read_the_same_token_orphan_the_first_rename`'s replay:

```
 8. Login A: revoke: DELETE /api/cli/tokens/P removes its row (:263; dashboard/src/modules/identity/organization-token.server.ts:174-177), and revoke returns Ok(true) (:264, :146)
    mcp.json: TB | Cloud: P agent revoked, TA agent live, TB agent live | Login A: Reporting { left: None }, read P | Login B: Listing, read P | named: none
 9. Login B: revoke: GET /api/cli/tokens does not list P, whose row another login's DELETE removed, so revoke returns Ok(false) and login says nothing of P (:255-261, :147)
    mcp.json: TB | Cloud: P agent revoked, TA agent live, TB agent live | Login A: Reporting { left: None }, read P | Login B: Reporting { left: None }, read P | named: none
10. Login A: prints "`ployz mcp` now acts with its own agent token TA in Organization …" (:171-174), while mcp.json holds TB, prints "Revoked the agent token P it replaced." (:175-177), and returns Ok
    mcp.json: TB | Cloud: P agent revoked, TA agent live, TB agent live | Login A: Done(Ok), read P | Login B: Reporting { left: None }, read P | named: none
11. Login B: prints "`ployz mcp` now acts with its own agent token TB in Organization …" (:171-174), and returns Ok
    mcp.json: TB | Cloud: P agent revoked, TA agent live, TB agent live | Login A: Done(Ok), read P | Login B: Done(Ok), read P | named: none
```

Some tests print a story before they fail, and some print none. To see the story of a property that breaks, put this test at the end of `mod tests`, before the file's last closing brace, with the model and property you want:

```rust
    #[test]
    fn scratch_story() {
        let model = fixed_without(Broken { fix_unlocked: true, ..Broken::default() });
        breaks(model, EVERY_TOKEN_ACCOUNTED);
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=mcp_login::tests::scratch_story)' --no-capture
```

Expected: `1 test run: 1 passed`, and a story of 8 steps from no `mcp.json`: A reads none, mints TA, renames and returns; B reads none before A's rename, mints TB and renames over TA, which is left live and unnamed. `breaks` prints the first path in BFS order whose last state breaks the property, and panics with "a path breaks the property" when none does. For a `sometimes`, such as a `COVERAGE` row's name, it prints the first path that reaches the row. Delete the test before you commit.

The story's words come from `reads_as`, which prints the statement each step copies. An edit to a model step needs `reads_as` edited with it, or the story's labels go stale.

## Real bug or model error

A counterexample is a lead, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `McpLogin::offers` and `actions`, which decide what is offered in each state;
  - `McpLogin::read`, `mint`, `save`, `unmake`, `list`, `delete` and `report`, one per step;
  - `McpLogin::fate`, which maps its `Broken` flags and the token's `Place` onto the facts `ployz_decide::fate::fate` takes;
  - `listed_as_agent`, the model's reading of `revoke`'s check and of `listCredentials`'s scope.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

## Breaking a guard yourself

Each `Broken` flag is already set by its own test, so setting one in a test of your own changes no other test and turns none red. To check the flag table below, run `scratch_flag_kinds`. To break a guard in a new test, set its flag through `as_is_without` or `fixed_without`; that changes one model, so only the tests that set the flag see it. An edit to the model's code changes every model at once (the K-table below). Add `--no-fail-fast` to the nextest command, so every red test is listed.

**The flags.** Each is the guard's statement and what replaces it; each test fails the named `always` property.

| Guard | Flag | The model's statement under the flag | Property | Test |
|---|---|---|---|---|
| The temp file's `sync_all` before the rename (`:75-78`) | `persist_unflushed_bytes` | `McpLogin::save` records `bytes: false` | `REVOKED_ONLY_WHEN_DURABLE` | `the_temp_file_is_flushed_before_the_rename` |
| `ployz_decide::fate::fate`'s Unflushed arm | `revoke_after_unflushed` | `McpLogin::fate` reads an unflushed save as `Durability::Durable` | `REVOKED_ONLY_WHEN_DURABLE` | `an_unflushed_save_keeps_the_token_before` |
| `ployz_decide::fate::fate`'s Cloud check | `fate_ignores_cloud` | `McpLogin::fate` reads a `Place::OtherCloud` token as `Previous::Same` | `EVERY_TOKEN_ACCOUNTED` | `fate_leaves_a_token_in_another_cloud` |
| `ployz_decide::fate::fate`'s Organization check | `fate_ignores_organization` | `McpLogin::fate` reads a `Place::OtherOrganization` token as `Previous::Same` | `EVERY_TOKEN_ACCOUNTED` | `fate_leaves_a_token_in_another_organization` |
| `revoke`'s listed-as-agent check (`:256-262`) | `revoke_unlisted` | `McpLogin::list` sets `found` to true | `NO_CLI_TOKEN_REVOKED` | `revoke_deletes_only_a_listed_agent_token` |
| The kind check and refusal (`:109`, `:119-124`) | `keep_non_agent` | `McpLogin::save` saves a `cli` token | `ONLY_AGENT_TOKEN_HELD` | `the_kind_check_refuses_a_cli_token` |
| The revoke of the token just made, and its warning (`:125-136`) | `failed_save_keeps_new_token` | `McpLogin::unmaking` returns `Pc::Done(Outcome::Err)` | `EVERY_TOKEN_ACCOUNTED` | `a_failed_save_revokes_the_token_just_made` |
| The fix's lock from the read | `fix_unlocked` | `McpLogin::locks` is false | `EVERY_TOKEN_ACCOUNTED` | `the_fix_needs_the_lock_from_the_read` |
| The fix's lock through the report | `fix_releases_before_report` | `McpLogin::save` releases the lock after the rename | `NOTE_HOLDS_WHEN_PRINTED` | `the_fix_needs_the_lock_through_the_report` |

**Which kind of property each flag breaks**, from a scratch run that checked each flag on `fixed()`. It lists the `always` properties that break on the flagged model but not on `fixed()`, and the rows `fixed()` reaches but the flagged model doesn't. The code guards run on `fixed()` too, because on the code as it is `EVERY_TOKEN_ACCOUNTED` already breaks through the race.

| Flag on `fixed()` | States | `always` properties that break | `sometimes` rows left unreached |
|---|---|---|---|
| `persist_unflushed_bytes` | 1114 | `REVOKED_ONLY_WHEN_DURABLE` | none |
| `revoke_after_unflushed` | 1366 | `REVOKED_ONLY_WHEN_DURABLE` | "a save is Unflushed, and the login warns that it may not survive a crash and that the token before is still live" |
| `fate_ignores_cloud` | 1178 | `EVERY_TOKEN_ACCOUNTED` | "the token before is in another Cloud, and the login warns that it is still live" |
| `fate_ignores_organization` | 1178 | `EVERY_TOKEN_ACCOUNTED` | "the token before is in another Organization, and the login warns that it is still live" |
| `revoke_unlisted` | 1166 | `NO_CLI_TOKEN_REVOKED` | "the token before is of kind cli, and the login leaves it and says nothing" |
| `keep_non_agent` | 1686 | `ONLY_AGENT_TOKEN_HELD` | "Cloud answers a cli token, and the login refuses it and revokes it" |
| `failed_save_keeps_new_token` | 694 | `EVERY_TOKEN_ACCOUNTED` | "Cloud answers a cli token, and the login refuses it and revokes it", "a save fails before the rename, and the login revokes the token it just made" and "the revoke of the token just made fails, and the login warns \"Could not revoke the token … just made\"" |
| `fix_unlocked` | 4104 | `EVERY_TOKEN_ACCOUNTED` and `NOTE_HOLDS_WHEN_PRINTED` | none |
| `fix_releases_before_report` | 2354 | `NOTE_HOLDS_WHEN_PRINTED` | none |

The scratch test that printed it follows. Put it at the end of `mod tests`, before the file's last closing brace, and run it alone.

```rust
    #[test]
    fn scratch_flag_kinds() {
        let flags = [
            ("persist_unflushed_bytes", Broken { persist_unflushed_bytes: true, ..Broken::default() }),
            ("revoke_after_unflushed", Broken { revoke_after_unflushed: true, ..Broken::default() }),
            ("fate_ignores_cloud", Broken { fate_ignores_cloud: true, ..Broken::default() }),
            ("fate_ignores_organization", Broken { fate_ignores_organization: true, ..Broken::default() }),
            ("revoke_unlisted", Broken { revoke_unlisted: true, ..Broken::default() }),
            ("keep_non_agent", Broken { keep_non_agent: true, ..Broken::default() }),
            ("failed_save_keeps_new_token", Broken { failed_save_keeps_new_token: true, ..Broken::default() }),
            ("fix_unlocked", Broken { fix_unlocked: true, ..Broken::default() }),
            ("fix_releases_before_report", Broken { fix_releases_before_report: true, ..Broken::default() }),
        ];
        let baseline = check(fixed());
        for (name, broken) in flags {
            let model = fixed_without(broken);
            let checker = check(model);
            println!("{name} on fixed: {} states", checker.unique_state_count());
            for property in model.properties() {
                let found = checker.discovery(property.name).is_some();
                match property.expectation {
                    Expectation::Always if found && baseline.discovery(property.name).is_none() => {
                        println!("  always breaks: {}", property.name);
                    }
                    Expectation::Sometimes if !found && baseline.discovery(property.name).is_some() => {
                        println!("  sometimes unreached: {}", property.name);
                    }
                    _ => {}
                }
            }
        }
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=mcp_login::tests::scratch_flag_kinds)' --no-capture
```

Expected: `1 test run: 1 passed`, with the lines the table above gives.

**The K-table.** Each edit below was made alone in the model at this PR's head, run with this command, and reverted:

```
cd core && cargo nextest run -p ployz-verify mcp_login --no-fail-fast
```

Each quoted statement occurs once in the function or const its row names. Most also occur once in `core/crates/ployz-verify/src/mcp_login.rs`, but not all: K12's `state.lock = None;` is also in `McpLogin::save`'s early release, and K19 to K24's `Start::…,` lines are also in `McpLogin::start`, `reads_as` and the tests. In the table, `\|` stands for `|`: unescape it when you copy K05, K06, K25 or K26.

| Edit, by function | Tests that went red |
|---|---|
| K01. `McpLogin::save`: `bytes: !self.broken.persist_unflushed_bytes,` replaced by `bytes: true,`, so `persist_unflushed_bytes` does nothing | `the_temp_file_is_flushed_before_the_rename` |
| K02. `McpLogin::fate`: `if durable || self.broken.revoke_after_unflushed {` replaced by `if durable {` | `an_unflushed_save_keeps_the_token_before` |
| K03. `McpLogin::fate`: `Place::OtherCloud if !self.broken.fate_ignores_cloud =>` replaced by `Place::OtherCloud =>` | `fate_leaves_a_token_in_another_cloud` |
| K04. `McpLogin::fate`: `Place::OtherOrganization if !self.broken.fate_ignores_organization =>` replaced by `Place::OtherOrganization =>` | `fate_leaves_a_token_in_another_organization` |
| K05. `McpLogin::list`: `self.broken.revoke_unlisted \|\| listed_as_agent(state.token(state.previous(who)));` replaced by `listed_as_agent(state.token(state.previous(who)));` | `revoke_deletes_only_a_listed_agent_token` |
| K06. `McpLogin::save`: `if state.token(made).kind != Kind::Agent && !self.broken.keep_non_agent {` replaced by `if state.token(made).kind != Kind::Agent {`, and in `McpLogin::offers`, `let saves = state.token(Tok::of(who)).kind == Kind::Agent \|\| self.broken.keep_non_agent;` replaced by `let saves = state.token(Tok::of(who)).kind == Kind::Agent;` | `the_kind_check_refuses_a_cli_token` |
| K07. `McpLogin::unmaking`: `if self.broken.failed_save_keeps_new_token {` replaced by `if false {` | `a_failed_save_revokes_the_token_just_made` |
| K08. `McpLogin::locks`: its body `self.fix && !self.broken.fix_unlocked` replaced by `self.fix` | `the_fix_needs_the_lock_from_the_read` |
| K09. `McpLogin::save`: `if self.locks() && self.broken.fix_releases_before_report {` replaced by `if false {` | `the_fix_needs_the_lock_through_the_report` |
| K10. `McpLogin::read`: the three lines `if self.locks() {`, `state.lock = Some(who);` and `}` deleted, so the fix takes no lock | `the_fix_holds`, `the_fix_holds_off_a_second_login`, `the_fix_needs_the_lock_through_the_report`, `the_fix_releases_the_lock_when_a_login_fails`, `the_fix_serializes_the_finding_s_logins` |
| K11. `McpLogin::offers`: `if pc == Pc::Idle && self.locks() && state.lock.is_some() {` replaced by `if false {`, so no login waits on the lock | `the_fix_holds`, `the_fix_holds_off_a_second_login`, `the_fix_needs_the_lock_through_the_report`, `the_fix_serializes_the_finding_s_logins` |
| K12. `McpLogin::step`: the three lines `if matches!(next, Pc::Done(_)) && state.lock == Some(who) {`, `state.lock = None;` and `}` deleted, so a returned login keeps the lock | `the_fix_holds`, `the_fix_holds_off_a_second_login`, `the_fix_releases_the_lock_when_a_login_fails`, `the_fix_revokes_a_first_login_s_token`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named`, `the_fix_serializes_the_finding_s_logins` |
| K13. `const CLI_ANSWERS: u8 = 1;` replaced by `const CLI_ANSWERS: u8 = 0;`, so Cloud never answers kind `cli` | `current_code_breaks_only_the_finding`, `the_fix_holds`, `the_fix_releases_the_lock_when_a_login_fails`, `the_kind_check_refuses_a_cli_token` |
| K14. `const SAVE_FAILURES: u8 = 1;` replaced by `const SAVE_FAILURES: u8 = 0;`, so no save fails before its rename | `a_failed_save_revokes_the_token_just_made`, `current_code_breaks_only_the_finding`, `the_fix_holds`, `the_fix_releases_the_lock_when_a_login_fails` |
| K15. `const FLUSH_FAILURES: u8 = 1;` replaced by `const FLUSH_FAILURES: u8 = 0;`, so no directory flush fails | `an_unflushed_save_keeps_the_token_before`, `current_code_breaks_only_the_finding`, `the_fix_holds`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named` |
| K16. `const LIST_FAILURES: u8 = 1;` replaced by `const LIST_FAILURES: u8 = 0;`, so no list fails | `current_code_breaks_only_the_finding`, `the_fix_holds` |
| K17. `const DELETE_FAILURES: u8 = 1;` replaced by `const DELETE_FAILURES: u8 = 0;`, so no `DELETE` fails | `current_code_breaks_only_the_finding`, `the_fix_holds` |
| K18. `const MAX_LOGINS: usize = 2;` replaced by `const MAX_LOGINS: usize = 1;`, so one login runs | `a_login_that_reads_the_other_s_token_makes_its_note_stale`, `current_code_breaks_only_the_finding`, `the_first_orphan_is_a_race`, `the_first_stale_note_follows_another_login_s_rename`, `the_fix_holds`, `the_fix_holds_off_a_second_login`, `the_fix_needs_the_lock_from_the_read`, `the_fix_needs_the_lock_through_the_report`, `the_fix_releases_the_lock_when_a_login_fails`, `the_fix_revokes_a_first_login_s_token`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named`, `the_fix_serializes_the_finding_s_logins`, `the_unraced_stale_note_is_the_second_route`, `two_first_logins_orphan_the_first_rename`, `two_logins_that_read_the_same_token_orphan_the_first_rename` |
| K19. `STARTS`: the line `Start::Unreadable,` deleted and `[Start; 6]` read as `[Start; 5]` | `current_code_breaks_only_the_finding`, `the_fix_holds` |
| K20. `STARTS`: the line `Start::Cli,` deleted and `[Start; 6]` read as `[Start; 5]` | `current_code_breaks_only_the_finding`, `revoke_deletes_only_a_listed_agent_token`, `the_fix_holds` |
| K21. `STARTS`: the line `Start::OtherCloud,` deleted and `[Start; 6]` read as `[Start; 5]` | `current_code_breaks_only_the_finding`, `fate_leaves_a_token_in_another_cloud`, `the_fix_holds`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named` |
| K22. `STARTS`: the line `Start::OtherOrganization,` deleted and `[Start; 6]` read as `[Start; 5]` | `current_code_breaks_only_the_finding`, `fate_leaves_a_token_in_another_organization`, `the_fix_holds`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named` |
| K23. `STARTS`: the line `Start::Here,` deleted and `[Start; 6]` read as `[Start; 5]` | `a_login_that_reads_the_other_s_token_makes_its_note_stale`, `current_code_breaks_only_the_finding`, `the_fix_releases_the_lock_when_a_login_fails`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named`, `the_fix_serializes_the_finding_s_logins`, `two_logins_that_read_the_same_token_orphan_the_first_rename` |
| K24. `STARTS`: the line `Start::Absent,` deleted and `[Start; 6]` read as `[Start; 5]` | `current_code_breaks_only_the_finding`, `the_fix_holds`, `the_fix_revokes_a_first_login_s_token`, `two_first_logins_orphan_the_first_rename` |
| K25. `accounted`: `\|\| token.named_left \|\| token.named_unrevoked` replaced by `\|\| token.named_unrevoked`, so a `Left` warning names nothing | `a_token_is_accounted_only_when_held_revoked_or_named`, `fate_leaves_a_token_in_another_cloud`, `fate_leaves_a_token_in_another_organization`, `only_a_race_orphans_a_token`, `the_first_orphan_is_a_race`, `the_fix_holds`, `the_fix_needs_the_lock_from_the_read`, `the_fix_needs_the_lock_through_the_report`, `the_fix_revokes_the_holder_s_token_and_keeps_the_earlier_one_named` |
| K26. `accounted`: `\|\| !token.live \|\| token.named_left \|\| token.named_unrevoked` replaced by `\|\| !token.live \|\| token.named_left`, so the failed-save warning names nothing | `a_token_is_accounted_only_when_held_revoked_or_named`, `fate_leaves_a_token_in_another_cloud`, `fate_leaves_a_token_in_another_organization`, `only_a_race_orphans_a_token`, `the_first_orphan_is_a_race`, `the_fix_holds`, `the_fix_needs_the_lock_from_the_read`, `the_fix_needs_the_lock_through_the_report` |
| K27. `McpLogin::save`: `state.raced = true;` deleted, so the `raced` mark is never set | `fate_leaves_a_token_in_another_cloud`, `fate_leaves_a_token_in_another_organization`, `only_a_race_orphans_a_token`, `the_first_orphan_is_a_race`, `the_fix_needs_the_lock_from_the_read`, `two_first_logins_orphan_the_first_rename`, `two_logins_that_read_the_same_token_orphan_the_first_rename` |
| K28. `McpLogin::save`: `state.file = File::Holds(made);` wrapped in `if match state.login(who).previous { Some(previous) => state.file == File::Holds(previous), None => !matches!(state.file, File::Holds(_)) } { … }`, so the rename happens only while `mcp.json` holds what its login read | `two_first_logins_orphan_the_first_rename`, `two_logins_that_read_the_same_token_orphan_the_first_rename` |

K10, K11 and K12 edit the fix's code directly; K08 is the same as deleting the lock everywhere. Run with `scratch_unreached` on `fixed()`: K10 gives 4104 states, and K11 5838, and under each `EVERY_TOKEN_ACCOUNTED` and `NOTE_HOLDS_WHEN_PRINTED` break, which is where `the_fix_holds` fails. K12 gives 176 states, breaks no `always`, and leaves the `Reach::Fix` row and "every login returns Ok" unreached, because the logins deadlock. K13 to K18 are the bounds dropped by one, and K13 to K17 are the choices removed. K19 to K24 are the start files removed. K25 to K27 break the properties' own bookkeeping, and K27 must turn the unraced searches red, which it does through `only_a_race_orphans_a_token` (the guard tests that search unraced go red too, since their filter then finds the race first). Under K28 the losing login renames nothing but still goes on to revoke its token before, so its own token is left unaccounted; only the replays, which pin `mcp.json`'s end, go red.

A side-effect failure is a test whose pinned path or expected rows your edit already moved, such as `fate_leaves_a_token_in_another_cloud` under K25. A guard that no longer earns its keep fails with its flag alone, on a model with no other edit.

**Making a `COVERAGE` row unreachable.** Put `false && ` right after the closure's parameters, so the whole body is the right operand. For a row whose closure is one expression:

```rust
        reached: |_, state| false && took(state, Branch::Read(File::Absent)),
```

For a row whose closure is a block:

```rust
        reached: |_, state| false && {
            took(
                state,
                Branch::Unmade {
                    why: Unmake::Refused,
                    revoked: true,
                },
            )
        },
```

The first is "a login reads no mcp.json"; the second is "Cloud answers a cli token, and the login refuses it and revokes it". Each turned `current_code_breaks_only_the_finding` and `the_fix_holds` red. Then run:

```
cd core && cargo nextest run -p ployz-verify mcp_login --no-fail-fast
```

Each of the 21 rows was made unreachable alone this way at this PR's head. Each `Reach::Both` row turned `current_code_breaks_only_the_finding` and `the_fix_holds` red, each `Reach::AsIs` row only `current_code_breaks_only_the_finding`, and the `Reach::Fix` row only `the_fix_holds`. No other test went red.

**Listing unreached rows.** To see every row a model leaves unreached in one run, put this at the end of `mod tests`:

```rust
    #[test]
    fn scratch_unreached() {
        for (name, model) in [("as is", as_is()), ("fixed", fixed())] {
            let checker = check(model);
            println!("{name}: {} states", checker.unique_state_count());
            for property in model.properties() {
                let found = checker.discovery(property.name).is_some();
                match property.expectation {
                    Expectation::Always if found => println!("  always breaks: {}", property.name),
                    Expectation::Sometimes if !found => println!("  unreached: {}", property.name),
                    _ => {}
                }
            }
        }
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=mcp_login::tests::scratch_unreached)' --no-capture
```

Expected at this head: `as is: 4104 states` with `EVERY_TOKEN_ACCOUNTED`'s and `NOTE_HOLDS_WHEN_PRINTED`'s strings under "always breaks", then `fixed: 1114 states`, and no row unreached.

## Bounds and blind spots

**The bounds, and the row or test each needs.** Each was dropped by one (K13 to K18) and lifted by one in scratch, alone, and run with `scratch_unreached` and the `mcp_login` suite.

| Bound | Value | Dropped by one: states, and unreached rows | Lifted by one |
|---|---|---|---|
| `MAX_LOGINS` | 2 | 91 and 91; every `Reach::AsIs` row and the `Reach::Fix` row | see below |
| `CLI_ANSWERS` | 1 | 2752 and 652; "Cloud answers a cli token, and the login refuses it and revokes it" | 4266 and 1192 states; no new `always` breaks; every row reached; the suite passes |
| `SAVE_FAILURES` | 1 | 3028 and 690; "a save fails before the rename, and the login revokes the token it just made" | 4188 and 1168; the same |
| `FLUSH_FAILURES` | 1 | 2836 and 802; "a save is Unflushed, and the login warns that it may not survive a crash and that the token before is still live" | 4224 and 1134; the same |
| `LIST_FAILURES` | 1 | 3336 and 962; "revoke's list fails, and the login warns that the token before is still live" | 4160 and 1122; the same |
| `DELETE_FAILURES` | 1 | 2992 and 704; "the revoke of the token just made fails, and the login warns \"Could not revoke the token … just made\"" and "revoke's DELETE fails once, and the login warns that the token before is still live" | 4188 and 1154; the same |

The state counts are as is, then fixed. Each drop's `always` results are unchanged: the two breaks as is, none on the fix (with `MAX_LOGINS` at 1, none at all). Dropping a start file leaves its row unreached:

| Start dropped | States | Unreached |
|---|---|---|
| `Unreadable` | 3939 and 1035 | "a login reads an unreadable mcp.json, warns \"Replacing it.\" and goes on with none" |
| `Cli` | 3159 and 879 | "the token before is of kind cli, and the login leaves it and says nothing" |
| `OtherCloud` | 3583 and 943 | "the token before is in another Cloud, and the login warns that it is still live" |
| `OtherOrganization` | 3583 and 943 | "the token before is in another Organization, and the login warns that it is still live" |
| `Here` | 2673 and 827 | the two revoke-side `Reach::AsIs` rows, since only a `P` both logins read can be revoked twice; as is only |
| `Absent` | 3939 and 1035 | "a login reads no mcp.json" |

The lifts ran `scratch_unreached`, then the suite with `--no-fail-fast`; it lists every red test, though only the `MAX_LOGINS` lift turned any red.

**The exact edits.** A drop or lift of a failure bound edits its const, such as `const CLI_ANSWERS: u8 = 1;` to `= 0;` or `= 2;`. No other const moves with it. `MAX_LOGINS` is the one derived bound: `TOKENS` is `MAX_LOGINS + 1` and moves with it, and `as_is()` runs `MAX_LOGINS` logins, so editing `const MAX_LOGINS: usize = 2;` to `3` is the whole lift. `letter` names slots 0 to 2, so a lift to 4 also needs a `3 => 'D'` arm. Remove a start by deleting its line from `STARTS` and lowering the array's length.

**Three logins.** With `MAX_LOGINS` at 3, run in release, or drop `--release`: the debug build took about 7 s of wall time on another host, since the dev profile already optimizes the model:

```
cd core && cargo nextest run -p ployz-verify -E 'test(=mcp_login::tests::scratch_unreached)' --no-capture --release
```

It printed `as is: 363558 states` and `fixed: 15615 states`, the same two `always` breaking as is, none on the fix, and no row unreached; it took 4.1 s of nextest time and 9.3 s of wall time with the release build warm. In the `mcp_login` suite at 3, `two_first_logins_orphan_the_first_rename` and `two_logins_that_read_the_same_token_orphan_the_first_rename` go red: their replays leave Login C unstarted, so `every_token_accounted` holds at their end. What the third login adds, as a scratch search showed: three overlapping first logins leave two tokens unaccounted (TA and TB, each replaced by the next rename), and no path leaves three.

**Left out, and argued.**
- **Kills, power loss and lost replies (risk 2).** A kill between `new_token` and the rename, or between the rename and the warnings, leaves a token unnamed. A reply lost after Cloud minted is an Err to the CLI (`call` and `exchange` retry nothing, `core/crates/ployz/src/cloud_account/calls.rs` and `core/crates/ployz/src/cloud_account/http.rs`; `REQUEST_TIMEOUT`, `core/crates/ployz/src/cloud_account/http.rs`), and the login returns at `:108` naming nothing. Each leaves a token unnamed on the fix too, so it is not this finding. A scratch lift added a kill action that ends any started login and releases its lock, and counted a killed login as returned: on `fixed()`, 3168 states, `EVERY_TOKEN_ACCOUNTED` broke, the other four `always` held, and a search that rejected every state with a killed login found no break of any of the five.
- **The unreadable file's token (risk 3).** `login` replaces a `Corrupt` file without revoking or naming the token it held (`:94-97`). Nobody can know that token, so the property counts none (`counts`).
- **A token Cloud refuses while `mcp.json` still holds it, and no `ployz mcp logout` (risk 5).** Expiry, `ployz token rm` and a member leaving are not steps; nor is any way back to no file.
- **The other writers of a token row** (What the model copies).
- **A pending Server Clear for `P`.** It makes the second `DELETE` succeed, so both logins print "Revoked …". `P` is revoked either way, so it changes no property's verdict and adds only a second "Revoked" note. A Clear is pending only when `P` had Server Access on a Server that hasn't confirmed the Clear (`pendingServerRevocations`, `dashboard/src/modules/machines/server-access.server.ts:387-405`).
- **A `POST` that fails before Cloud inserts a row**, and a read error other than not found or `Corrupt` (`:55`, `:98`). The login returns before it mints or writes anything.
- **A change of sign-in or Organization between logins.** `cloud.json` is the sign-in store's model. Both logins act as one signed-in person in one Organization, who can list and revoke their own tokens.
- **Expired rows.** `listCredentials` lists expired tokens too (`dashboard/src/modules/identity/organization-token.server.ts:157`), and the model has no clock.
- **Each bound above.**

## Follow-ups

Not filed:
- **The revoke-side misreport.** Its two outcomes and their conditions are in The finding. No property covers it.
- **Risk 2's unnamed tokens** on both models (Left out).
- **The re-read before persist**, which E-142 must argue (The fix).

## Keep the model in step

When a change touches a function the description names, reread the step that copies it, rerun the nextest command, and compare each story row with the line it cites. The rows of What the model copies give each step's lines. If the code changed:
- edit the model step and its `reads_as` arm together;
- rerun the K-table and the row kills, and update their red sets;
- rerun both greps in What the model copies, and update their output;
- move the sync commit below only once the model describes the code at the new commit.

## Coverage

| Covered | Functions |
|---|---|
| `core/crates/ployz/src/handlers/mcp/login.rs` | `login`, `stored`, `save`, `Saved`, `fate`, `revoke`, `warnings`, `Left`, `Stored`, `path`, `LIFETIME_DAYS` |
| `core/crates/ployz-decide/src/fate.rs` | `fate`, `Previous`, `Durability`, `Why`, `Fate` |
| `core/crates/ployz/src/handlers/account.rs` | `in_cloud` |
| `core/crates/ployz/src/handlers/mod.rs` | `runtime` |
| `core/crates/ployz/src/cloud_account/token.rs` | `new_token`, `credentials`, `remove_token` |
| `core/crates/ployz/src/handlers/mcp.rs` | `publishing_setup`, `agent` (readers, left out) |
| `dashboard/src/routes/api/cli/-cli.handler.ts` | `GET tokens`, `POST tokens`, `DELETE tokens/:id`, `pendingRevocation` |
| `dashboard/src/modules/identity/organization-token.server.ts` | `createOrganizationToken`, `mintOrganizationTokenUnchecked`, `listCredentials`, `revokeCredential` |

Sync commit: `f8e91a4d83`. The model describes the covered code as it is at that commit.
