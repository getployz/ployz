---
name: verify-sign-in-store
description: Model-check this device's Cloud sign-in file, cloud.json, against two `ployz login` processes, another Cloud command's `signed_in`, `ployz logout` and the browser approving, denying or letting a code lapse, each stepping between any two of the others' reads and writes, with the Stateright model in core/crates/ployz-verify/src/sign_in_store.rs. Use when a change touches core/crates/ployz/src/cloud_login.rs (CredentialStore's load, replaceable, save, clear and set_organization; signed_in, start, wait, logout, request_code, poll_once, poll, finish, session, forget), core/crates/ployz/src/handlers/login.rs (login and logout), the callers of signed_in in core/crates/ployz/src/cloud_account.rs and core/crates/ployz/src/handlers/mcp.rs, or the device authorization wiring in dashboard/src/server/auth.server.ts.
---

# Verify the Cloud sign-in store

The model in `core/crates/ployz-verify/src/sign_in_store.rs` explores every interleaving of the processes that read and write `cloud.json`, this device's Signed-in Device (`dashboard/CONTEXT.md:120-122`) or the code it waits on. A bare `:n` below is a line of the file named just before it in the same parentheses, and otherwise of `core/crates/ployz/src/cloud_login.rs`. Every other path is written in full from the repo root, except the better-auth paths, which are inside its npm package (Cloud's side).

## What the model copies

**The processes.** Each one is its own `ployz` process, so one can step between any two of another's file operations. No lock covers them.
- **Two `ployz login` processes**, slots Login A and Login B. Without `--json`, `ployz login` waits (`core/crates/ployz/src/handlers/login.rs:83`). It runs `start` (`:304-341`), called at `core/crates/ployz/src/handlers/login.rs:85`, then `wait` (`:348-364`), called at `core/crates/ployz/src/handlers/login.rs:99`.
- **One other Cloud command**, slot Command. With no pasted token, `credential` calls `signed_in` (`core/crates/ployz/src/cloud_account.rs:84-91`); `core/crates/ployz/src/handlers/mcp.rs:692` calls it too. `signed_in` (`:276-287`) loads the file, returns a SignedIn, and polls a Pending once with `poll_once` (`:282`), which can finish or forget it.
- **One `ployz logout`**, slot Logout (`:387-446`).
- **The browser**, a signed-in member at `/device`, approves or denies a code. **The clock** lets a code lapse.

Each process runs once and ends. Running the same command again is a new process, which the model gives a slot of its own (Bounds).

**Every writer of `cloud.json`, and every caller that forgets it.**

| Writer | Line | Reached from | In the model |
|---|---|---|---|
| `request_code`'s save of a Pending | `:513` | `start`'s fall-through (`:340`) and its resume branch on Denied or Expired (`:332-334`) | `Pc::Requesting`, one step with its POST |
| `start`'s refresh of a stored SignedIn | `:321` | `start`, after get-session honours the token (`:314-323`) | `Pc::Refreshing` |
| `finish`'s save of a SignedIn | `:583` | `wait` (`:357`), and `poll_once` (`:521-532`) from `start`'s resume branch (`:326`) and from `signed_in` (`:282`) | `Pc::Finishing` |
| `set_organization`'s save of a SignedIn | `:248` | `ployz org use` and `ployz org new` (`core/crates/ployz/src/cloud_account.rs:425`, `:464`) | left out (Bounds) |
| `logout`'s `clear` of no or an unreadable file | `:398-400` | `logout` | `Pc::Clearing(File::Absent)` and `Pc::Clearing(File::Unreadable)` |
| `logout`'s `clear` after a Pending or a SignedIn | `:444` | `logout` | `Pc::Clearing` |
| `forget`'s `clear` | `:617-620` | `wait` on Denied or Expired (`:360-361`), `poll_once` on either (`:529-530`) | `Pc::Forgetting` |

`save` writes a `NamedTempFile` in the same directory and renames it over the file with `persist` (`:211-222`), so a reader sees the old file or the new one, never a partial one. `clear` is a bare `remove_file` that ignores NotFound (`:224-229`). It reads nothing, so it removes whatever the file holds at that moment, and the model's `forget` records no read on the code as it is (`forget_reads_nothing`).

**The reads.** `load` (`:188-201`) returns `Ok(None)` for no file and `Corrupt` for a file that is not a `Stored`. `replaceable` (`:204-209`) reads Corrupt as none. `start` and `logout` read through `replaceable`, and `signed_in` through `load`, so a Command fails on an unreadable file (`:277`) where a login starts over.

**Cloud's side.** Cloud answers the device-code flow with better-auth's device authorization plugin. The dashboard wires it at `dashboard/src/server/auth.server.ts:268-277`, setting only `verificationUri` and `validateClient` (`dashboard/src/server/auth.server.ts:269-272`), so better-auth's defaults hold: a code expires in 30 minutes and the poll interval is 5 s. The plugin is the npm package `better-auth@1.7.16`, pinned at `dashboard/pnpm-lock.yaml:4414`. To read its lines, fetch it into an empty directory and open `package/dist`:

```
mkdir /tmp/better-auth && cd /tmp/better-auth && npm pack better-auth@1.7.16 && tar xzf better-auth-1.7.16.tgz
```

Inside that package:
- `better-auth@1.7.16/dist/plugins/device-authorization/index.mjs:20-21` sets those defaults.
- `better-auth@1.7.16/dist/plugins/device-authorization/routes.mjs` answers the token poll in `redeemDeviceCode`, which starts at `better-auth@1.7.16/dist/plugins/device-authorization/routes.mjs:290`. In that function, by line of that file:
  - `:291-297` looks the row up, and `:298-299` answers `invalid_grant` when there is none;
  - `:303-307` answers `slow_down` when the row's `lastPolledAt` is less than the interval ago, before `:315` sets `lastPolledAt`, so a `slow_down` leaves it unchanged;
  - `:317-328` deletes an expired row and answers `expired_token`;
  - `:330-331` answers `authorization_pending`;
  - `:334-345` deletes a denied row and answers `access_denied`;
  - `:357` consumes an approved row (`consumeOne`), and `:371-372` answers `invalid_grant` if another request consumed it first;
  - `:456` opens a session for the token (`createSession`).
- `better-auth@1.7.16/dist/db/internal-adapter.mjs:23` gives a session 604800 s, 7 days, unless `session.expiresIn` is set. The dashboard's `session` option sets only `additionalFields` (`dashboard/src/server/auth.server.ts:392-394`).

So the first token request for an approved code spends it, and every later request finds no row and gets `invalid_grant`. `dashboard/src/server/auth.server.postgres.test.ts:259-260` pins that answer. `poll` reads `invalid_grant` and `expired_token` alike as Expired (`:556-564`, the comment at `:561`).

The model keeps the cases apart in `Grant` and `Answer`:
- **Spent** (`Grant::Spent`, `Answer::Spent`): the code was approved and another poll took its token. This is the finding's case.
- **Lapsed** (`Grant::Lapsed`, `Answer::Lapsed`): the code really expired. `poll` checks the client's own clock first and answers Expired without asking Cloud (`:535-536`). The model takes that check as the expiry; Cloud's `expired_token` (`better-auth@1.7.16/dist/plugins/device-authorization/routes.mjs:325-328`) gives the same answer.
- **Swept** (`Grant::Swept`, `Answer::Swept`): `access_denied` deleted the row, and a later poll gets `invalid_grant`.

`Grant::Spent { ended }` says whether a logout signed the token's session out at Cloud. `State::honoured` reads it.

**The finding.** Two `ployz login` processes wait on one code. A's `start` finds no file and saves Pending(c0). B's `start` reads it, polls once, gets `authorization_pending`, and waits too (`:325-331`). The browser approves. A's poll gets the token, and `finish` saves SignedIn(c0). B's next poll gets `invalid_grant` for the spent code, `poll` reads it as Expired (`:562`), and B's `forget` removes the file (`:361`, `:617-620`). A printed "Signed in to …", B printed "the sign-in code expired before it was approved" (`:67`), and `cloud.json` is gone while Cloud still honours A's session.

The property that breaks is `APPROVED_SIGN_IN_KEPT`: "once a process returned a sign-in Cloud still honours, cloud.json holds a SignedIn until a logout steps". `State::kept` marks every code whose token a process returned signed in since the last logout step, and the property asks for a SignedIn while any of those tokens is still honoured. A logout's read and its clear both reset it, because a logout means to remove the file. Watching only the last returned token would miss a live sign-in: in `a_stale_refresh_does_not_hide_a_live_sign_in`, with three logins, B signs in with c1, C's stale refresh then returns c0, which a logout already ended, and a Command's forget removes the file; c1 is still honoured and no file holds it.

The same broken state has four more routes, each with its own test:
- **A slow code request** (`a_slow_code_request_overwrites_an_approved_sign_in`). B's `start` reads the file before A saves, and B's POST to `/api/auth/device/code` outlasts A's approval, poll and save. B's `request_code` then saves its new Pending over A's SignedIn (`:513`).
- **The resume branch** (`a_late_resume_replaces_an_approved_sign_in`). B's `start` reads Pending(c0) before A saves, and its one poll lands after A's poll spent c0. B's `forget` removes A's SignedIn (`:530`), and its `request_code` saves a new Pending (`:333`).
- **Another Cloud command forgets** (`a_cloud_command_forgets_an_approved_sign_in`). Its `signed_in` loads Pending(c0) before A saves, and its one poll gets `invalid_grant` afterwards. Its `forget` removes the SignedIn (`:282`, `:530`).
- **Another Cloud command finishes, and the waiter forgets** (`a_waiter_forgets_a_sign_in_a_cloud_command_finished`). The Command's `signed_in` loads Pending(c0), and its one poll gets the token, so its `finish` saves SignedIn(c0) (`:282`, `:527`, `:568-585`). A's next poll gets `invalid_grant` for the code the Command spent, and A's `forget` removes the Command's sign-in (`:361`, `:617-620`). A prints "the sign-in code expired before it was approved".

**The timing condition.** The finding needs the loser's `clear` to land after the winner's save. The loser can poll before that save, get `invalid_grant` while the file still holds the Pending, and still clear after it, since `forget` runs after the poll returns. Each waiter sleeps its interval before each poll (`:355`); the interval starts as Cloud's reply floored at one second (`:511`), 5 s by better-auth's default. better-auth keeps one `lastPolledAt` per code and answers `slow_down` to a poll less than the interval after it, without moving it (`better-auth@1.7.16/dist/plugins/device-authorization/routes.mjs:303-315`). `wait` adds 5 s to its sleep on each `slow_down` (`:358`), and `poll_once` treats `slow_down` as still waiting (`:528`). So two waiters on one code take turns: the one whose poll lands inside the other's 5 s window backs off, and the other gets the token. On a real CLI against a local Cloud, every trial lost the file, and which terminal won depended on when B joined (#3847, Live lanes). The model has no clock, so it explores every order and says nothing about how often each happens.

**What a user sees when the finding happens.**
- **Terminal A** prints "Open <url> and confirm the code <code>." and "Waiting for approval... " (`core/crates/ployz/src/handlers/login.rs:98`, `:104-114`), then "Signed in to <cloud> as <email> in Organization <slug>." (`core/crates/ployz/src/handlers/login.rs:99-100`, `:116-123`).
- **Terminal B** prints the same code with no browser opened, since it resumed (`core/crates/ployz/src/handlers/login.rs:110-111`). It then fails with "the sign-in code expired before it was approved" (`:67`), with `next: ployz login` (`core/crates/ployz/src/failure.rs:1030-1033`).
- **The next Cloud command** with no `cloud.json` and no pasted token gets `SignedOut`, "Not signed in to Cloud." (`:65`, `:278`), with `next: ployz login` (`core/crates/ployz/src/failure.rs:1030-1033`).
- **Cloud still holds the winner's session.** The token's `createSession` (`better-auth@1.7.16/dist/plugins/device-authorization/routes.mjs:456`) opened it, and no step ended it. The token was only in the file the loser removed. `ployz logout` then finds no file, clears nothing and prints "Not signed in." (`:397-400`, `core/crates/ployz/src/handlers/login.rs:156`), so it cannot revoke the session. The session ends in one of two ways:
  - **It lapses 7 days after it opened**, since nobody holds the token to use it (`better-auth@1.7.16/dist/db/internal-adapter.mjs:23`, `dashboard/src/server/auth.server.ts:392-394`).
  - **`ployz token rm <id>` from a new sign-in**, the same computer's included. `ployz token ls` lists the caller's unexpired device sessions (`dashboard/src/modules/identity/organization-token.server.ts:137-145`). Every device is named "ployz login <date>", so an orphan is told apart from the current device only by its expiry and the `current` mark.

  The web offers no way to end it. Account → Sessions lists it with a CLI badge but no Sign out (`dashboard/src/routes/_protected/cloud/$organizationSlug/_org/-components/account-sections.tsx:107`), under "CLI devices sign out with `ployz logout`" (`:94`), and `revokeAccountSession` skips CLI sessions (`dashboard/src/modules/account/account.server.ts:57`). Organization → Tokens renders only tokens, no devices (`dashboard/src/routes/_protected/cloud/$organizationSlug/_org/-components/tokens-settings.tsx:28`, `:38-46`).

## Run it

```
cd core && cargo nextest run -p ployz-verify sign_in_store --no-capture
```

Expected: `19 tests run: 19 passed`. `current_code_breaks_only_the_finding` prints `as is: 114773 states`, and `the_fix_keeps_an_approved_sign_in` prints `fixed: 92647 states`. Each story prints under its test's name. A warm run takes about 3.5 s of nextest time. CI runs `ployz-verify` in its workspace nextest run.

Read the tests in four groups.
- **The finding, on the code as it is.**
  - `current_code_lets_a_losing_waiter_forget_an_approved_sign_in` replays the finding's nine actions with `follow`, asserts each move and the end state, and then asks the checker for its first path to that same state (`breaks_where`). The checker keeps one path to each state, the first in BFS order, and a shorter path reaches this one: B requests c0, and A's `start` resumes it after the approval and gets the token with its one poll. So the replay shows the finding's own schedule, and the second check shows the checker discovers its end state.
  - `a_slow_code_request_overwrites_an_approved_sign_in`, `a_late_resume_replaces_an_approved_sign_in`, `a_cloud_command_forgets_an_approved_sign_in` and `a_waiter_forgets_a_sign_in_a_cloud_command_finished` each keep to paths whose last step is the route's breaking step, taken from a state where `approved_kept` held and the file held a SignedIn.
  - `a_stale_refresh_does_not_hide_a_live_sign_in` replays the 17-action, three-login schedule above with `follow`, and checks that `approved_kept` is false at its end.
  - `current_code_breaks_only_the_finding` expects `APPROVED_SIGN_IN_KEPT` to break, `NO_TORN_READ` to hold, and every `COVERAGE` row that is not `Reach::Fix` to be reached.
- **The code's guard, and what `clear` reads.** `save_writes_a_temp_file_and_renames_it` sets `save_tears`, and expects `NO_TORN_READ` to break: a `start` reads the half-written file as none. `forget_reads_nothing` replays a `forget` that removes the file while B's save of Pending(c1) is half-written, and expects no torn read.
- **The fix.**
  - `the_fix_keeps_an_approved_sign_in` runs `fixed()` with `assert_properties`: both `always` properties hold, and every row that is not `Reach::AsIs` is reached.
  - `the_fix_keeps_the_finding_s_sign_in` replays the finding's schedule under the fix: B's `forget` leaves A's SignedIn.
  - `the_fix_rereads_after_the_resume_branch` replays the resume branch's schedule under the fix: B's `request_code` finds A's SignedIn where it expected Pending(c0), throws its code away, and `start` reads the file again, refreshes the SignedIn and returns it.
  - `the_fix_requests_after_forgetting_its_own_pending` replays a resume branch whose code the browser denied: B's `forget` removes its own Pending, so its `request_code` expects no file and saves Pending(c1) with no re-read.
  - `the_fix_forgets_only_the_polled_pending` checks the predicate on each edge state: no file, an unreadable file, another code's Pending, and a SignedIn. No step leaves the file unreadable once a process saved, so for the unreadable file it also steps one `forget` from a state built for it, under the fix and as the code is.
  - `the_fix_still_reports_expired` shows the fix's limit: the finding's loser keeps the SignedIn but still fails Expired.
- **The fix's parts.** `the_fix_needs_the_comparison`, `the_fix_needs_the_forget_lock`, `the_fix_compares_at_the_save` and `the_fix_needs_the_request_guard` each set one flag on `fixed_without(..)` (The fix).

## The fix

The fix is a design for a later PR. No product code has it yet.

```text
forget(polled)              under a lock: remove cloud.json only while it holds
                            Pending(polled); otherwise leave it
request_code(expected)      POST /api/auth/device/code without the lock; then, under
                            the lock, save the new Pending only while cloud.json still
                            holds `expected`; otherwise throw the new code away and
                            start reads cloud.json again
```

`expected` is the file as the process last knew it: what `start` read, or no file after the resume branch's `forget` removed its own Pending (`after_forget`).

The POST runs without the lock because holding it across a network call would block every other `ployz` process's `finish`, `forget` and `logout` for the POST's length. A mismatch costs one code no process knows, which Cloud keeps as a pending row until it expires 30 minutes later (`better-auth@1.7.16/dist/plugins/device-authorization/index.mjs:20`). The POST changes only that new row, which no other process reads, so the model takes the POST, the compare and the save as one step, and leaves a thrown-away code unminted. The compare must sit at the save, after the POST: one made before it and released for the POST leaves the slow code request's window open, which `fix_compares_before_post` shows.

| Part | What it keeps | Flag | Test, and the `always` property that breaks without it |
|---|---|---|---|
| The device-code comparison in `forget` | A waiter, a resume or a Command whose code another poll spent leaves the SignedIn that poll saved | `fix_forgets_any_file` | `the_fix_needs_the_comparison`, `APPROVED_SIGN_IN_KEPT` |
| The lock around `forget`'s compare and its removal | A save that lands between them. Without it, a `forget` reads its own Pending, a `finish` save lands, and `remove_file` removes the SignedIn | `fix_forget_unlocked` | `the_fix_needs_the_forget_lock`, `APPROVED_SIGN_IN_KEPT` |
| The guard on `request_code`'s save | A `start` that read the file before a login saved its SignedIn. That covers the slow code request and the resume branch: the resume branch's `forget` leaves A's SignedIn, and its `request_code` would replace it | `fix_request_overwrites` | `the_fix_needs_the_request_guard`, `APPROVED_SIGN_IN_KEPT`, by the slow request and by a replay of the resume branch |
| That guard's compare at the save, under the lock, after the POST | A save that lands while the POST runs. Without it, the compare finds the file `start` read, A's `finish` saves during the POST, and the Pending is written over the SignedIn | `fix_compares_before_post` | `the_fix_compares_at_the_save`, `APPROVED_SIGN_IN_KEPT` |

The resume branch needs no guard of its own: the request guard covers it, as `the_fix_rereads_after_the_resume_branch` shows.

The lock must also cover `finish`'s and `start`'s saves for the compare to mean anything, since those are the saves the compare races. In the model every write under the fix is one atomic step. Which lock the later PR takes, and where its file lives, is that PR's choice.

What the fix keeps, and what the later PR must decide:
- **The loser still fails Expired.** B's poll still gets `invalid_grant`, which `poll` reads as Expired (`:562`). With the fix, B's terminal still prints "the sign-in code expired before it was approved" while the file holds A's SignedIn (`the_fix_still_reports_expired`). The fix PR may want `forget` to report the SignedIn it left, or `wait` to read the file again on `invalid_grant`.
- **A refresh can save a token a logout ended.** `start` reads a SignedIn, get-session honours it, a logout signs it out and clears the file, and the refresh saves it back (`:314-323`). The row "start's refresh saves a SignedIn whose session a logout ended meanwhile" reaches it on both models. The next `start` finds the session ended and requests a code (`:315`, `:599-600`), and a Command returns the token without asking Cloud (`:279`).
- **A waiter can sign in after a logout cleared its Pending.** The row "a waiter signs in after a logout read its Pending and cleared cloud.json" reaches it on both models. The logout printed "Signed out of <cloud>." and the file then holds a SignedIn.
- **A logout can clear a sign-in saved after its read.** Logout reads Pending(c0), which it does not send to Cloud (`:402-407`). A's `finish` saves SignedIn(c0), and logout's `clear` removes it (`:444`). A printed "Signed in to …" and the logout "Signed out of <cloud>.", and Cloud keeps a session no file holds, the same orphan as the finding. The row "a logout's clear removes a SignedIn saved after its read" reaches it on both models. `APPROVED_SIGN_IN_KEPT` does not catch it, since a logout step resets `kept`. The later PR could make logout's `clear` compare, under the same lock, against what logout read.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. A replayed schedule (`follow`) prints "a schedule" in place of the property. For a `sometimes`, the story is cut at the first state that reaches it. `breaks` keeps that first path with a single-thread visitor and stops at the first discovery. `breaks_where` keeps the first path that also passes a filter on the path, and explores the whole model. One thread keeps the path the same from run to run.

These are the last two rows of `current_code_lets_a_losing_waiter_forget_an_approved_sign_in`'s first story, the `follow` replay of the finding:

```
 8. Login B: wait: after its sleep, poll: Cloud answers invalid_grant, because c0 is spent, and poll reads it as Expired (:562); forget is next (:360-361)
    cloud.json: SignedIn(c0) | Cloud: c0 Spent { ended: false } | Login A: Done(SignedIn) | Login B: Forgetting { code: c0, answer: Spent, then: Return } | Command: Idle | Logout: Idle
 9. Login B: forget: clear removes cloud.json, which held SignedIn(c0) (:617-618, :224-229); ployz login fails, "the sign-in code expired before it was approved", next: ployz login (:619, :67, core/crates/ployz/src/failure.rs:1030-1033)
    cloud.json: none | Cloud: c0 Spent { ended: false } | Login A: Done(SignedIn) | Login B: Done(Expired) | Command: Idle | Logout: Idle
```

The same test then prints a second story, 8 rows long: the checker's own first path to that end state, in which B requests c0 and A resumes it.

Each row is one step, named for who takes it: Login A, Login B, Login C (with three logins), Command, Logout, Browser or Clock. A bare `:n` in a row is a line of `core/crates/ployz/src/cloud_login.rs`. The line under it is the state after the step:
- `cloud.json` is what the file holds: none, unreadable, `Pending(cN)` or `SignedIn(cN)`. A SignedIn's code names the token it was exchanged for.
- `Cloud` lists each minted code and its `Grant`.
- Each process's `Pc` is its next step. `Requesting { expect }` is the file the fix's save must still find, and `None` on the code as it is.

A row that skips a guard says "(under Broken)" when a flag is set. A story that starts mid-path, as `a_late_resume_..`'s second print does, starts with "from the state above".

A test that fails inside `assert_properties` (`the_fix_keeps_an_approved_sign_in`) reports only the first property it finds broken, and prints the raw actions instead of the rows. In those actions, `Step(0)` is Login A, `Step(1)` Login B, `Step(2)` Login C, `Step(3)` the Command and `Step(4)` the logout (`MAX_LOGINS`, `COMMAND` and `LOGOUT`); `Approve(cN)`, `Deny(cN)` and `Lapse(cN)` are the browser's and the clock's. To see the rows, call `breaks` with that model and property in a scratch test.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites. Each `Pc`, `Branch` and `Answer` variant's doc comment names its lines, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `actions` and `offers`, which decide what is offered in each state;
  - `SignInStore::step`, for each process's steps;
  - `request`, `forget`, `after_forget` and `polls`, for the branches;
  - `poll`, for Cloud's answers.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

## Breaking a guard yourself

Set its `Broken` flag through `as_is_without` or `fixed_without`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every red test is listed.

Which kind of property each flag breaks, from a scratch run that checked each flagged model in full and listed the rows its unflagged model reaches but it does not:

| Flag | `always` properties that break | `sometimes` rows left unreached |
|---|---|---|
| `save_tears`, on the code as it is | `NO_TORN_READ`, beside the finding's `APPROVED_SIGN_IN_KEPT` | the six rows that read the file a save wrote at the save's own step, which now holds unreadable: "request_code saves a new Pending", both refresh rows, "a waiter signs in", "signed_in's poll_once gets the token and finish saves SignedIn" and "a waiter signs in after a logout read its Pending and cleared cloud.json" |
| `fix_forgets_any_file` | `APPROVED_SIGN_IN_KEPT` | the five fix rows where a `forget` leaves the file: "the fix's forget leaves a SignedIn", "… leaves another code's Pending", "… finds no cloud.json", "signed_in's forget leaves a SignedIn under the fix" and "under the fix, a waiter whose poll got invalid_grant still fails Expired beside the SignedIn" |
| `fix_forget_unlocked` | `APPROVED_SIGN_IN_KEPT` | none |
| `fix_compares_before_post` | `APPROVED_SIGN_IN_KEPT` | none |
| `fix_request_overwrites` | `APPROVED_SIGN_IN_KEPT` | "the fix's request_code finds cloud.json changed and start reads it again" |

The scratch test that printed it follows. Put it at the end of `mod tests` and run it like the lift snippet below, with `-E 'test(=sign_in_store::tests::scratch_flag_kinds)'` and without `--release`; it takes about 2 s of nextest time.

```rust
    #[test]
    fn scratch_flag_kinds() {
        let runs = [
            ("save_tears on as_is", as_is(), Broken { save_tears: true, ..Broken::default() }),
            ("fix_forgets_any_file on fixed", fixed(), Broken { fix_forgets_any_file: true, ..Broken::default() }),
            ("fix_forget_unlocked on fixed", fixed(), Broken { fix_forget_unlocked: true, ..Broken::default() }),
            ("fix_compares_before_post on fixed", fixed(), Broken { fix_compares_before_post: true, ..Broken::default() }),
            ("fix_request_overwrites on fixed", fixed(), Broken { fix_request_overwrites: true, ..Broken::default() }),
        ];
        for (name, base, broken) in runs {
            let baseline = check(base);
            let model = SignInStore { broken, ..base };
            let checker = check(model);
            println!("{name}: {} states", checker.unique_state_count());
            for property in model.properties() {
                let found = checker.discovery(property.name).is_some();
                match property.expectation {
                    Expectation::Always if found => println!("  always breaks: {}", property.name),
                    Expectation::Sometimes if !found && baseline.discovery(property.name).is_some() => {
                        println!("  sometimes unreached: {}", property.name)
                    }
                    _ => {}
                }
            }
        }
    }
```

An edit to the model's code changes every model at once. For this PR, each edit below was switched on alone in a scratch copy of `core/crates/ployz-verify/src/sign_in_store.rs`, run with `--no-fail-fast`, and reverted. "Login" means a `who` that is not `COMMAND`.

| Edit, by function | Tests that went red |
|---|---|
| B1. `compares`: `self.fix && ..` read as `(self.fix \|\| true) && ..`, so the code's `forget` compares too | 8: `current_code_lets_..`, `a_cloud_command_..`, `a_waiter_forgets_..`, `a_late_resume_..`, `a_stale_refresh_..`, `current_code_breaks_only_the_finding`, `forget_reads_nothing`, `the_fix_forgets_only_the_polled_pending` (its as-is step) |
| B2. `expect`: `(self.fix && ..).then_some(read)` read with `self.fix` true, so the code's `request_code` guards its save too | `a_slow_code_request_..` |
| B3. `save`: `if self.broken.save_tears` read as `if false` | `save_writes_a_temp_file_and_renames_it`, `forget_reads_nothing` (its replay needs the torn step) |
| B4. `save`: `if self.broken.save_tears` read as `if true` | 13, among them `current_code_breaks_only_the_finding` (`NO_TORN_READ`) |
| B5. `compares` returns false under the fix, so its `forget` removes whatever the file holds | 7: `the_fix_keeps_an_approved_sign_in`, `the_fix_keeps_the_finding_s_sign_in`, `the_fix_forgets_only_the_polled_pending`, `the_fix_needs_the_forget_lock`, `the_fix_needs_the_request_guard`, `the_fix_rereads_after_the_resume_branch`, `the_fix_still_reports_expired` |
| B6. `compares`: `!self.broken.fix_forgets_any_file` read as `true` | `the_fix_needs_the_comparison` |
| B7. `forget`: `self.broken.fix_forget_unlocked` (`core/crates/ployz-verify/src/sign_in_store.rs:822`) read as `false` | `the_fix_needs_the_forget_lock` |
| B8. `expect`: `!self.broken.fix_request_overwrites` read as `true` | `the_fix_needs_the_request_guard` |
| B9. `expect` returns `None` under the fix, dropping the request guard | `the_fix_keeps_an_approved_sign_in`, `the_fix_needs_the_comparison`, `the_fix_compares_at_the_save`, `the_fix_rereads_after_the_resume_branch` |
| B10. `forgets`: `file == File::Pending(polled)` read as `matches!(file, File::Pending(_))` | `the_fix_forgets_only_the_polled_pending`, `the_fix_keeps_an_approved_sign_in` |
| B11. `forget`, right after `let removes = ..`: under the fix, a Login whose `then` is `Then::Return` (the wait loop's `forget`; the resume branch's has `Then::Request`) that leaves a SignedIn returns `Pc::Done(Outcome::SignedIn)` and records no branch | `the_fix_still_reports_expired`, `the_fix_keeps_an_approved_sign_in`, `the_fix_keeps_the_finding_s_sign_in` |
| B12. `request`: `self.broken.fix_compares_before_post` (`core/crates/ployz-verify/src/sign_in_store.rs:867`) read as `false` | `the_fix_compares_at_the_save` |
| B13. `saved`: `state.kept = [false; MAX_CODES];` before the SignedIn's bit is set, so the property watches only the last returned token | `a_stale_refresh_does_not_hide_a_live_sign_in` |
| B14. `forget`: on the code as it is, a Login's `forget` leaves a SignedIn (`removes` false when the file holds one) | `current_code_lets_..`, `a_late_resume_..`, `a_waiter_forgets_..` |
| B15. `after_forget`: `if removed` read as `if false`, so a resume branch that removed its own Pending still expects it | `the_fix_requests_after_forgetting_its_own_pending` |
| B16. `forget`: `if self.compares()` read as `if true`, so the code's `forget` reads the file before its `clear` | `forget_reads_nothing` |

A side-effect failure is a test whose pinned path or expected rows your edit already moved. A guard that no longer earns its keep fails with its flag alone, on a model with no other edit.

Each of the 36 `COVERAGE` rows was then made unreachable alone, with its predicate replaced by one that never holds. Every row turned the test that expects it red: `current_code_breaks_only_the_finding` for a `Both` or `AsIs` row, `the_fix_keeps_an_approved_sign_in` for a `Both` or `Fix` row, and only those.

A scratch test inside `sign_in_store::tests` can call the module's private helpers. Put it at the end of `mod tests`, run it alone with a nextest filter that matches only its name, and delete it before you commit. The snippet below lifts the login bound to three:

```
cd core && cargo nextest run -p ployz-verify -E 'test(=sign_in_store::tests::scratch)' --no-capture --release
```

```rust
    #[test]
    fn scratch() {
        let fixed3 = SignInStore { logins: 3, ..fixed() };
        let checker = check(fixed3);
        println!("fixed, 3 logins: {} states", checker.unique_state_count());
        checker.assert_properties();
        let as_is3 = SignInStore { logins: 3, ..as_is() };
        let checker = check(as_is3);
        println!("as is, 3 logins: {} states", checker.unique_state_count());
        assert!(checker.discovery(APPROVED_SIGN_IN_KEPT).is_some());
        assert!(checker.discovery(NO_TORN_READ).is_none());
        for row in COVERAGE.iter().filter(|row| row.reach != Reach::Fix) {
            assert!(checker.discovery(row.name).is_some(), "{} is never reached", row.name);
        }
    }
```

Expected: it passes and prints `fixed, 3 logins: 8114298 states`, then `as is, 3 logins: 12083579 states`. The test takes about 45 s of nextest time; a cold release build of the crate's dependencies adds about a minute before it.

## Bounds and blind spots

The bounds, and why each leaves out no new state:
- **Two logins by default.** `MAX_LOGINS` is 3, so a scratch model can run three (the snippet above). With three, the fix still holds both `always` properties and reaches every row, and the code as it is still breaks only `APPROVED_SIGN_IN_KEPT` and reaches every row that is not `Reach::Fix`.
- **One Command and one logout, each once.** A Command's steps are a subset of a resume: it reads the file, polls once, and finishes, forgets or returns (`:276-287`, `:521-532`). It never requests a code. A second Command would add a second one-shot poller, and the three-login lift already adds a second resume poller beside the first. A second logout reads and clears as the first does, and the property already resets at the first. Neither bound was lifted.
- **At most one minted code per login.** Only `post_and_save` mints, and it saves the code at once; a login that saved its Pending waits and never starts again. Under the fix a login can reach `request_code` more than once: its resume branch's `forget` leads to it, and a re-read leads back to `start`, which can reach it again. A re-read throws its code away before `mint`, so the model never mints it (`the_fix_rereads_after_the_resume_branch` ends with only c0 minted). `MAX_CODES` is `MAX_LOGINS`.
- **The model has no clock.** The wait loop polls only once Cloud would answer otherwise: an `authorization_pending` poll changes nothing (`:359`), so `offers` holds a waiter back while its code waits, and the model has no step for `slow_down`. That leaves out timing, not orders: every order of polls is explored. On the real CLI, `slow_down` decides which order happens (The timing condition).

The other processes can step between any two of a process's file operations. Each read and write in the model, and why its steps are the code's:
- **`start`'s read and get-session** (`:307`, `:315`) are one step. A logout can end the session between them. In the model the logout's step comes first: it ends the session and leaves the file for its own clear, so `start` reads the SignedIn and finds it ended, as it would.
- **`start`'s refresh save** (`:321`) is its own step, `Pc::Refreshing`. A logout can sign out and clear between the read and it, which the refresh row above reaches.
- **`start`'s read and `request_code`** (`:307`, `:340`) are separate steps, `Pc::Requesting`, so any process can write between them. That is the slow code request's window.
- **`request_code`'s POST and save** (`:494-513`) are one step. The POST changes only Cloud's new row, which no other process knows until the save names it, so nothing another process does between them depends on it. Under the fix the compare joins them at the save; under `fix_compares_before_post` the compare is its own step, and the POST and save are another (`Pc::Posting`).
- **`poll_once`'s poll** (`:326`, `:282`) and the **wait loop's sleep and poll** (`:355-356`) are one step each. A sleep touches nothing.
- **`finish`'s get-session and save** (`:574-583`) are one step, after the poll's step. Only the process that polled holds the token, and the file holds a Pending until this save, so no logout can sign the session out before it (`:402-407`).
- **`forget`'s `clear`** (`:617-618`) is one step and reads nothing. Under the fix, its compare and removal are one step under the lock, or two under `fix_forget_unlocked` (`Pc::Removing`).
- **`signed_in`'s `load`** (`:277`) is its own step.
- **`logout`'s read and its POST to `/api/cli/logout`** (`:397`, `:409-414`) are one step, and its `clear` (`:444`) is another. The POST ends only the session of the token logout read, and every other process reads that session only through a get-session; a read between the two orders the same way before the logout's step.

Left out:
- **`set_organization`** (`:239-250`) saves the SignedIn its caller got from `signed_in` moments before, with a new Organization (`:248`), for `ployz org use` and `ployz org new` (`core/crates/ployz/src/cloud_account.rs:425`, `:464`). It always leaves a SignedIn, so it cannot remove one. But it is a stale write: it can put an older SignedIn over a newer one, or over a Pending a login just saved, which the model does not explore.
- `OtherCloud` (`:308-313`), and a Pending of another Cloud, which `start` treats as no file (`:338`). The model has one Cloud.
- A pasted token, with which `credential` never reads the file (`core/crates/ployz/src/cloud_account.rs:89-100`).
- `ployz login --json` without `--wait`, which returns after `start` (`core/crates/ployz/src/handlers/login.rs:89-97`). It is a login that never takes its wait step, which the model reaches.
- `logout`'s unreachable, 401 and 403 replies (`:415-430`). Each still clears the file (`:444`); the model's logout always ends a SignedIn's session.
- Errors: `finish`'s get-session failing, or answering 401, which `finish` returns as Ended (`:574-576`, `:599-600`), Io from `save` or `clear`, and a reply `poll` cannot read (`:563`).
- How long any step takes, and `slow_down`.

A green model says nothing about them. Cover the code ones with:

```
cd core && cargo nextest run -p ployz cloud_login
```

Expected: `8 tests run: 8 passed`.

## Keep the model in step

When a change alters how a process reads or writes `cloud.json`, or how Cloud answers the token poll, change the model in the same PR. Examples: a lock, a compare before a write, a new writer, a new caller of `signed_in`, a retry on `invalid_grant`.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `start`: the `Pc::Idle` arm of `step` for a login, `Pc::Refreshing` and `Pc::Resuming`.
   - `request_code`: `request`, `post_and_save` and `Pc::Posting`.
   - `wait`, `poll_once` and `poll`: `polls`, `poll` and `offers`.
   - `finish`: `Pc::Finishing` and `saved`.
   - `forget` and `clear`: `forget`, `forgets`, `after_forget` and `Pc::Removing`.
   - `save`: `save` and `Pc::Torn`.
   - `signed_in`: the `COMMAND` arm of `step`.
   - `logout`: the `LOGOUT` arm of `step` and `Pc::Clearing`.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change ships the fix, model it as the code writes it and make it the default. Then turn the finding tests into a `Broken` flag that restores the old `forget` and save, and keep the tests, since their printed paths are the record of what the fix keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names (`took`).
5. Every `State` field is read: `file`, `grants`, `pcs`, `kept` (by `approved_kept`), `torn_read` (by `NO_TORN_READ`) and `took` (by the rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployz/src/cloud_login.rs`: `CredentialStore`'s `load`, `replaceable`, `save`, `clear` and `set_organization`; `signed_in`, `start`, `wait`, `logout`, `request_code`, `poll_once`, `poll`, `finish`, `session` and `forget`; and `LoginError`'s messages.
- `core/crates/ployz/src/handlers/login.rs`: `login`, `announce`, `report` and `logout`.
- `core/crates/ployz/src/cloud_account.rs`: `credential`, and the `set_organization` callers.
- `core/crates/ployz/src/handlers/mcp.rs`: its `signed_in` call.
- `core/crates/ployz/src/failure.rs`: the `next` for `SignedOut`, `Expired`, `Denied` and `Ended`.
- `dashboard/src/server/auth.server.ts`: `cliSignInPlugins` and its `deviceAuthorization` options, and the `session` options.
- `dashboard/pnpm-lock.yaml`: the `better-auth` version, and with it `redeemDeviceCode`'s answers and the session expiry.

Sync commit: `a19b1f07b6b9dca214fe9488214458d3129aa4cb`, the `main` this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
