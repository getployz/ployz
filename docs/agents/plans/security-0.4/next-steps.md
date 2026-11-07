# Next steps

[plan.md](plan.md) is the execution plan. [decisions.md](decisions.md) is the decision record. This file records what changed after the X, Y and E0 reports and Nick's corrections, and what runs next. It adds to the plan and does not replace it.

## Changes since the plan was written

1. **Goal.** Hosted Ployz Cloud must be clearly safer than self-hosting the dashboard on your own VPS. Root on np1 must not be able to steal secrets, start containers, change settings, take data, or add servers or Devices. The decisions.md promise now says this.
2. **GitHub keeps broad permissions.** The App keeps write access to code, workflows and settings for the future coding agent. The App key moves off np1 into a token minter. np1 receives only read tokens and a few fixed operations. Every other write needs a customer signature. See decisions.md, "GitHub App: broad permissions, key off np1".
3. **Y found our own org exposed.** The `getployz*` App installs reach the release environment, the ployz.sh Cloudflare token and `getployz/build`. Fixing this is in progress in its own thread.
4. **X found Hosted DNS runs on np1.** It holds the Route 53 and ACME credentials, so np1 can skip the API entirely. The recommendation is to move it off np1, with changes accepted only when signed by the name's pinned customer key.
5. **E0 answered the Move question.** The source Machine decides: it persists one handoff decision, and the destination only becomes Writer with the source's signed handoff receipt ([e0-move-report.md](e0-move-report.md)). This goes into A's contract and thread E.
6. **Devices already exist.** They are Full-scope keys that only an existing Device can add, and the first one is approved over SSH. Thread A builds the trusted key list on Devices rather than a parallel key system. The holes A1 closes still stand: a joining Machine takes its Devices from Cloud, Register wraps the Sealing Key to any Machine key, and a mesh peer can reach the API as Full.

## Rules from the earlier discussion that the plan must keep

- **Build the shared foundation once, then parallelize.** Otherwise the browser, Volume Runs and GitHub each invent their own approval system.
- **First vertical slice is a signed CLI restart.** It goes end to end through Device signing, transport, Machine verification and replay records before B, C and E start building.
- **Never turn on deploy enforcement before the GitHub rule and build verification ship with it.** Pushes must never start needing a tap.
- **At most three implementation threads active at once.**
- **Immediate hardening runs early and alone.** It does not wait for the foundation:
  - Pin the Ingress Proxy image. Today the validator accepts the submitted image (`ployz-core/src/domain/ingress.rs:75`).
  - Bind Cloud's upgrade selection to the customer's channel (`ployzd/src/installer/release.rs:132`).
  - Enable `requireSignature` on Better Auth `bearer()`, so a database copy of `session.token` no longer signs in (`dashboard/src/server/auth.server.ts:268`).
  - Stop storing GitHub OAuth tokens in `account`.

## Order

| Wave | Runs | Gate to next wave |
| --- | --- | --- |
| Now | Org lockdown (its own thread, interactive). Immediate hardening. Thread A contract plus the signed CLI restart slice. | CLI restart works on Boat. Cloud cannot forge it, replay it, add a Device or register a Sealing Key recipient. |
| Next | B (browser sessions and MCP proposals), C (deploy payload), E (Volume Runs with E0's design). The GitHub minter and DNS separation design run alongside. | C1 and C2 per plan.md |
| Then | D (GitHub auto-deploy rule and build proof), previews. C's deploy enforcement turns on together with D. | C3, then C4 |
| Last | F coverage and cutover, R red team | C5, release |

## Open decisions

The open decisions table in plan.md still holds. These three are new or sharper:

1. **First Device for dashboard-only customers.** Recommendation: SSH approval stays the rule. During first server install, the install command also prints a short code the user matches against their browser.
2. **Session length. Decided 2026-11-07: 60 minutes per Environment** for deploy, settings, start, stop and restart. Volume and other destructive workflows are approved one at a time, and an agent proposal always asks once. The session grant gets its own 60-minute cap. Each button press is still a separate statement with the 15-minute cap from #3926, so no single approval lives longer than today.
3. **Minter and DNS hosting.** Which cloud account, who can deploy it, and how its own deploys are kept out of np1's reach.
