# Account (ACC)

Who you are and what you may do in Ployz Cloud: signing in from a browser and from the CLI, your Account page (profile, sessions, passkeys, emails), Organizations and their settings, members and roles, Organization Tokens, the yes a person gives before something is removed, billing and Production, deleting an Organization, and the Agent sandbox an agent makes before anyone signs in. Verifiers start on a verify-dashboard instance (`dashboard/scripts/verify/up.sh`, signed in as Ada, owner of `ada`; `as.sh grace` is an admin, `as.sh linus` a member) and drive the CLI against it with the seed's tokens (`PLOYZ_TOKEN=$(jq -r .cliToken .verify/run/seed.json)`, `PLOYZ_CLOUD_URL` from `.verify/run/info`). Signing in with GitHub itself goes to real github.com, so the seeded cookie stands in for it. Agent tools, the Agent sidebar and approvals an agent asks for are in [cli-and-agents.md](cli-and-agents.md); Integrations (Tailscale, Cloudflare) and the cluster name are in [networking.md](networking.md); Forget all servers and devices are in [servers.md](servers.md).

### ACC-01 Sign in to Ployz Cloud with GitHub
- **Surface:** dashboard
- **Reach:** `/auth` → **Continue with GitHub**; first sign-in lands in a new Organization named after you (like "Ada's Projects") on **Add your app**
- **Proof:** after GitHub returns, the URL is `/cloud/<new-slug>` and the account menu's **Switch organization** lists that Organization; a failed return shows **Sign-in didn't finish** with **Try again**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: a real GitHub account and an OAuth app pointed at the local Cloud (FAKE_GITHUB=1 doesn't cover sign-in)
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** getting-started/quick-start.md#sign-in

### ACC-02 Sign the CLI in with `ployz login`
- **Surface:** CLI
- **Reach:** `PLOYZ_CLOUD_URL=<local> ployz login`, then in the browser open `/device`, check the code and click **Approve** on **Sign in the Ployz CLI**; also **Deny**
- **Proof:** terminal returns signed in; `ployz --json ctx` shows `credential: "device"` and the Organization the page named; `ployz token ls` lists a KIND `device` row named `ployz login <date>`; the page reads **Your terminal is signed in**. Deny reads **Sign-in denied** and the CLI exits non-zero; reusing a code reads **This code was already used. Run ployz login again.**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (seeded cookie approves the code); verify-sign-in-store for two logins, logout and the browser racing over cloud.json
- **Compat:** CLI→Cloud: a computer signed in before November 2026 gets `Cloud ended this device's sign-in` once and must run `ployz login` again (see ACC-05)
- **Docs:** cli/overview.md#sign-in

### ACC-03 Finish a pending sign-in from a script (`--json`, `--wait`, `--cloud-url`)
- **Surface:** CLI
- **Reach:** `ployz --json login --cloud-url <local>` (prints page and code at once), approve in the browser, then `ployz login --wait`
- **Proof:** first call's JSON holds the approval page and code and exits without waiting; `--wait` returns once approved and `ployz --json ctx` shows `credential: "device"`; a `retry:` line for a `--cloud-url` with a user, path, query or fragment shows it as `--cloud-url '<CLOUD_URL>'`
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard; verify-sign-in-store
- **Compat:** CLI→Cloud: 0.3.19's login flow against a 0.4 Cloud should still finish (not checked)
- **Docs:** **undocumented** (only --help): `--json` and `--wait` on `ployz login`

### ACC-04 Sign this computer out with `ployz logout`
- **Surface:** CLI
- **Reach:** `ployz logout`; repeat with Cloud unreachable (`PLOYZ_CLOUD_URL` to a closed port, or stop vite) and after leaving the sign-in's Organization
- **Proof:** `ployz --json ctx` then shows `credential: null` and `organization = (none: not signed in)`; the `device` row is gone from `ployz token ls` (run from another sign-in) and from **Account → Sessions**; offline it still forgets the sign-in and warns Cloud may honour it until `ployz token rm`; agents connected only from this sign-in are disconnected (AGT-05)
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard; verify-sign-in-store
- **Compat:** none
- **Docs:** cli/overview.md#sign-in

### ACC-05 An old or unreadable sign-in is replaced
- **Surface:** CLI
- **Reach:** corrupt `$PLOYZ_CONFIG`'s sibling `cloud.json`, then `ployz status` and `ployz login`; with it unreadable, `PLOYZ_TOKEN=<token> ployz status` without `PLOYZ_CLOUD_URL`
- **Proof:** the unreadable sign-in is forgotten and `ployz login` writes a new one; with `PLOYZ_TOKEN` alone the command asks for `PLOYZ_CLOUD_URL`; a pre-November-2026 sign-in fails with `Cloud ended this device's sign-in` while Organization Tokens keep working
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard; verify-sign-in-store
- **Compat:** CLI→Cloud: sign-ins a 0.3.x CLI saved before November 2026 are ended once
- **Docs:** cli/overview.md#sign-in

### ACC-06 Keep the sign-in where you choose (`PLOYZ_CONFIG`, `XDG_CONFIG_HOME`)
- **Surface:** CLI
- **Reach:** `XDG_CONFIG_HOME=/tmp/x ployz login`; `PLOYZ_CONFIG=/tmp/y/config.toml ployz login`; `XDG_CONFIG_HOME=rel ployz login`
- **Proof:** the sign-in lands in `/tmp/x/ployz/`, then `/tmp/y/`; a relative `XDG_CONFIG_HOME` is ignored and `~/.config/ployz` is used; `--ployz-config https://…` is refused as a URL
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** cli/overview.md#sign-in

### ACC-07 See your profile and sign-in methods
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/account` (account menu → **Account**); `/cloud/account` redirects there
- **Proof:** **Profile** shows Ada Lovelace, ada@example.com and each linked sign-in with its date, read-only; the avatar stays highlighted while the page is open; a user in no Organization still gets `/cloud/account` without the Organization shell
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/account.md#profile

### ACC-08 Sign a browser out from Sessions
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/account` → **Sessions**; sign in a second browser session (another `agent-browser --session`), then **Sign out** on its row
- **Proof:** rows list most recent first, this browser marked **This device**, CLI sign-ins marked **CLI** with no button; after **Sign out** the row is gone on reload and the other session's next load lands on `/auth`; agents only that browser connected leave **Connected agents**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/account.md#sessions

### ACC-09 Add a passkey on the Account page
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/account` → **Passkeys** → add one (agent-browser virtual authenticator)
- **Proof:** the passkey shows in the list after reload; a deploy page then offers **Approve with passkey** instead of `Your servers don't trust your passkey`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 (servers must trust the passkey)
- **Compat:** Cloud→Server: a Server on an older ployz refuses with `Upgrade web-1 first. Its ployz can't check your passkey.`
- **Docs:** servers/manage-servers.md#add-a-passkey (account/account.md lists three sections and omits Passkeys)

### ACC-10 Choose which emails you get
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/account` → **Notifications**: set **Uptime incidents**, **Failed GitHub deploys**, **Failed automatic upgrades**, **Crashed services** to **Email** or **None**
- **Proof:** each row starts at **Email**; a pick saves at once and survives reload; the same choices show from another Organization (`/cloud/grace/~/account` as Grace is per-user, not per-Organization)
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud (MAIL=1, else rows are read-only)
- **Drive:** verify-dashboard MAIL=1
- **Compat:** none
- **Docs:** account/notifications.md

### ACC-11 Turn an email off from the email itself
- **Surface:** dashboard
- **Reach:** trigger a crash or uptime email (OBS-31, OBS-32) under MAIL=1, open it in Mailpit, click **Turn off … emails**; or POST the `List-Unsubscribe` URL (`/api/mail/unsubscribe`)
- **Proof:** the matching row on **Account → Notifications** reads **None** after reload, without a sign-in; the invite email has no **Turn off** link
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard MAIL=1 SERVERS=1
- **Compat:** none
- **Docs:** account/notifications.md#turn-an-email-off-from-the-email

### ACC-12 A Cloud without email says so
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/account` → **Notifications** on a plain `up.sh` (no MAIL), and on a Cloud without the `observe` profile
- **Proof:** rows are disabled with **Email isn't set up on this Cloud**; without log shipping the **Crashed services** row says **Crash emails need Cloud log shipping**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/notifications.md#on-a-self-hosted-ployz-cloud

### ACC-13 Failed GitHub deploy and failed upgrade emails
- **Surface:** dashboard
- **Reach:** push a commit that fails to build to a connected repository (FAKE_GITHUB=1 webhook); let an automatic upgrade fail
- **Proof:** Mailpit gets a **Failed GitHub deploys** email naming the failed service, the commit and a logs link; a deploy started by hand sends none; a deploy that leaves services waiting names each one and what it waits on
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a failing automatic upgrade needs a release channel the lab can serve (release mirror)
- **Drive:** verify-dashboard MAIL=1 FAKE_GITHUB=1 SERVERS=1
- **Compat:** none
- **Docs:** account/notifications.md

### ACC-14 List and switch Organizations from the CLI
- **Surface:** CLI
- **Reach:** `ployz org ls`; `ployz org use grace` (signed in as a user in two Organizations)
- **Proof:** `org ls` lists each Organization you can act in; after `org use`, `ployz --json ctx` names the new Organization while the dashboard stays where it was, and switching in the dashboard doesn't move the CLI
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** cli/overview.md#sign-in (`org use`); `ployz org ls` is **undocumented** (only --help)

### ACC-15 Make another Organization
- **Surface:** CLI
- **Reach:** `ployz org new "Acme Corp"`, `ployz org new "Acme Corp" --slug acme` (twice); also account menu → **Switch organization → New organization** → **Create**
- **Proof:** prints the slug it picked, with a short suffix the second time; `ployz --json ctx` acts in it; the dashboard lands in it with you as **Owner** on **Members**; with `PLOYZ_TOKEN` set it's refused and asks for `ployz login`
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/organizations.md#your-organization

### ACC-16 Rename the Organization
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=general` → **Display name**, type and press Enter; repeat as Linus (`as.sh linus`)
- **Proof:** the new name shows in the sidebar after reload and the URL keeps `/cloud/ada`; as a member the field is read-only
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/organizations.md#your-organization

### ACC-17 Invite someone with a link
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=members` → **Invite**, pick **Admin**, add an email, **Make link**; open the link at `/invite/<token>` as another user and click **Join**
- **Proof:** with MAIL=1 the dialog says where it emailed and Mailpit has it; without, the dialog says to send it yourself; the joiner appears on **Members** with that role without reload; reopening the link reads **This invite was used already**; a member already in sees **You're in**; **Revoke** makes it read **This invite was revoked**; after 7 days **This invite expired**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (MAIL=1 for the email)
- **Compat:** none
- **Docs:** account/organizations.md#invite-people

### ACC-18 Manage the team from the CLI (`ployz org member`, `ployz org invite`)
- **Surface:** CLI
- **Reach:** `ployz org member ls`; `ployz org member invite bob@example.com --role admin`; `ployz org member role grace@example.com member`; `ployz org member rm linus@example.com`; `ployz org invite ls`; `ployz org invite revoke bob@example.com`
- **Proof:** `invite` prints a link (and says it emailed under MAIL=1); `role`, `rm` and `revoke` show the change and the Organization and ask yes or no, and without a terminal exit with the `--approval` retry; `invite ls` as Linus is refused; two members with one name refuse with `ambiguous`, list both emails, exit 2; through `ployz mcp` an agent is told to do it in Ployz Cloud
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (`PLOYZ_TOKEN` per seeded user)
- **Compat:** CLI→Cloud: published commands; a 0.3.19 CLI fetches the manifest too, so check it lists them
- **Docs:** cli/overview.md#manage-your-team

### ACC-19 The last owner stays
- **Surface:** CLI
- **Reach:** as the only owner: `ployz org member role ada@example.com admin`, `ployz org member rm ada@example.com`, `ployz org leave`; dashboard **Leave** on your own row
- **Proof:** each refuses at once, before any yes is asked; the dashboard's **Leave** and role select refuse the same; after making Grace an owner, Ada can leave
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/organizations.md#invite-people

### ACC-20 Change a role or remove a member in the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=members`: role select beside Linus → **Admin**; **Remove** on Linus; as Grace (admin) try to change Ada (owner)
- **Proof:** no yes/no is asked; Linus's role reads Admin after reload; once removed, Linus's next page load refuses the Organization, his `linusToken` gets refused, and reopening his invite reads **Membership ended**; Grace can't make, change or remove an owner
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/organizations.md#invite-people

### ACC-21 Leave an Organization
- **Surface:** CLI
- **Reach:** as Grace: `ployz org leave`, `ployz org leave --yes`; also **Leave** on your own row in the dashboard
- **Proof:** asks first unless `--yes`; prints the Organization the device moved to; `ployz --json ctx` names it; Grace's tokens in `ada` stop working at once; connected agents in `ada` are disconnected
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** cli/overview.md#manage-your-team

### ACC-22 Ployz says which role can do it
- **Surface:** CLI
- **Reach:** as Linus (`PLOYZ_TOKEN=$(jq -r .linusToken …)`): `ployz env protect production --off`, `ployz server ls`, `ployz deploy --help`; in the dashboard as Linus open **Organization → General**
- **Proof:** refusals read `Only an owner or admin of this Organization can do this.` or `Server access needs an admin. Ask an owner or admin of ada.` with exit 1; `ployz deploy --help` ends with `Needs changes:deploy`; a config change the role can't make reads `Needs environment:delete`
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/organizations.md#invite-people

### ACC-23 Let Ployz support in
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/settings?section=general` → **Allow support access** on, then the **Variables**, **Config files** and **Container logs** switches
- **Proof:** switches start off and turning access off turns all three off (after reload); a support session sees a banner and sealed-looking variables, file names without contents, and no container logs until each switch is on; support can't delete the Organization; a self-hosted Cloud (no BILLING) shows no setting
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud (BILLING=1, a hosted Cloud) · External: a Ployz support identity on the local Cloud
- **Drive:** verify-dashboard BILLING=1
- **Compat:** none
- **Docs:** account/organizations.md#let-ployz-support-in

### ACC-24 Make an Organization Token with a preset
- **Surface:** CLI
- **Reach:** `ployz token new ci --preset deploy --env staging`; `ployz token new ro --preset read`; `ployz token new full --expires-in 400`; also `/cloud/ada/~/settings?section=tokens` → **New token** (**Full as you**, **Read only**, **Deploy** + Environment)
- **Proof:** the secret prints once; `ployz token ls` shows SCOPE `Deploy staging` and KIND `cli`; `--expires-in 400` is refused (1–365); a Deploy token for a Protected Environment is refused for Linus; in an Organization with no Environment the dialog starts at **Read only**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** cli/overview.md#make-an-organization-token

### ACC-25 A Deploy or Read token is held to its preset
- **Surface:** CLI
- **Reach:** with a staging Deploy token: `ployz up --project shop --env production`, `ployz logs --env production`, `ployz env shutdown --env staging`, `ployz server ls`; with a Read token: `ployz set web.replicas=2`
- **Proof:** production attempts fail with `This token can deploy only to staging.` and change nothing; `server ls` fails with `This token's grant doesn't include servers:shell.`; shutdown and pruning deploys need a Full token; the Read token changes nothing
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** account/permissions.md#narrow-a-token-to-one-job

### ACC-26 List and revoke tokens and signed-in computers
- **Surface:** CLI
- **Reach:** `ployz token ls`, `ployz --json token ls`; `ployz token rm ci`; make two tokens named `ci` and `ployz token rm ci`; `ployz token rm <device-id>`
- **Proof:** rows show OWNER (`you` or a teammate, `Full as Grace`), SCOPE, KIND `cli`/`agent`/`device`; `--json` gives `owner.{id,name,email,you}`; an admin sees everyone's tokens, a member only theirs; duplicate names list IDs and remove neither; removing a device ID signs that computer out (its next command needs `ployz login`)
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** CLI→Cloud: a Cloud older than 0.3.2 leaves OWNER empty
- **Docs:** cli/overview.md#make-an-organization-token

### ACC-27 A token can't make or revoke tokens
- **Surface:** CLI
- **Reach:** `PLOYZ_TOKEN=<full token> ployz token new x`; `… ployz token rm <other>`; `ployz login` with `PLOYZ_TOKEN` still set, then `ployz token new x`
- **Proof:** each is refused; with `PLOYZ_TOKEN` set Ployz keeps acting as the token after login, until `unset PLOYZ_TOKEN`; `ployz logout` leaves tokens you made working
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** cli/overview.md#make-an-organization-token

### ACC-28 `PLOYZ_TOKEN` tolerates paste whitespace
- **Surface:** CLI
- **Reach:** `PLOYZ_TOKEN="  $T\n" ployz status`; `PLOYZ_TOKEN="ab\ncd" ployz status`
- **Proof:** the padded token works; the broken one fails with `PLOYZ_TOKEN is not a token` and the value never prints
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** cli/overview.md#make-an-organization-token

### ACC-29 Say yes before something is removed
- **Surface:** CLI
- **Reach:** a deploy that removes a service: `ployz service rm worker && ployz deploy` in a terminal; again with stdin not a TTY; again with `--json`; then rerun with `--approval <DIGEST>` after another deploy lands in between
- **Proof:** terminal asks `Go ahead with …? [y/N]` and anything but y changes nothing; no terminal prints the list and the `--approval DIGEST` command; `--json` exits with `approval_required` and `details.retry`; a stale digest says `It changed after your yes` and runs nothing; `ployz env sync --value ROW` refuses `--approval`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`); verify-approval-binding for the carried digest
- **Compat:** CLI→Cloud: an older ployz that sends an approval ID is refused with the update to run
- **Docs:** account/organizations.md#destructive-actions-ask-first

### ACC-30 Answer approvals from the CLI (`ployz org approval`)
- **Surface:** CLI
- **Reach:** have an agent ask for a deploy (AGT-10), then `ployz org approval ls`, `ployz org approval show <id>`, `ployz org approval approve <id>` in a terminal, `ployz org approval deny <id> --reason "not now"`
- **Proof:** `ls` lists the pending approval with what it would do; `show` reads pending, then approved/denied; approve runs the waiting command; deny passes the reason to the agent; approving your own request is refused; `verify-cluster cli` (no TTY) refuses approve for want of a terminal
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`--cloud`; `verify-cluster approve`/`deny`); verify-approval-binding
- **Compat:** CLI→Cloud: published command
- **Docs:** **undocumented**: only named in cli/overview.md#commands-from-ployz-cloud; behavior in --help

### ACC-31 Upgrade to Production and manage billing
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~/billing` → **Upgrade to production**, **Manage billing**
- **Proof:** the page shows the server count and the monthly price ($10 with two servers, $5 per extra); without BILLING=1 the route is a not-found with **Go home**; as Grace (admin) billing actions are refused (owners only)
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: Polar checkout is fake, so completing a purchase can't be verified
- **Drive:** verify-dashboard BILLING=1
- **Compat:** none
- **Docs:** account/organizations.md#billing

### ACC-32 Without Production, uptime and analytics ask for it
- **Surface:** dashboard
- **Reach:** BILLING=1 instance: service **Monitoring** tab, `/cloud/ada/shop/production/analytics`, `ployz uptime show web`
- **Proof:** Monitoring shows **Uptime is part of Production** where status and **Response time** would be, metrics charts still render; Analytics shows **Analytics is part of Production** with a **Billing** link; `uptime show` answers `needs_production`; a self-hosted (no BILLING) Cloud shows both to everyone
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard BILLING=1
- **Compat:** none
- **Docs:** account/organizations.md#billing

### ACC-33 Delete an Organization from the dashboard
- **Surface:** dashboard
- **Reach:** `/cloud/<org>/~/settings?section=general` → **Delete organization**, type the slug, **Delete**
- **Proof:** the button is disabled while projects exist and for non-owners; after delete the dashboard opens another Organization, or **Create an organization** with none left; Servers are unpaired; with a Server offline the Organization stays disabled and **Try again** finishes it once the Server is back
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 (`verify-cluster power off` for the offline case)
- **Compat:** Cloud→Server: unpairing clears Cloud's key and every device key on each Server
- **Docs:** account/organizations.md#delete-an-organization

### ACC-34 Delete an Organization with `ployz org rm`
- **Surface:** CLI
- **Reach:** `ployz org rm acme --confirm acme`; without a terminal; with `--approval <DIGEST>`
- **Proof:** refuses while projects remain (`ployz project rm` first); always asks for a person's yes; afterwards the device moves to another Organization and says which, or asks for `ployz logout` then `ployz login` with none left; the slug is free for a new Organization
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`--cloud`)
- **Compat:** Cloud→Server: same unpairing as ACC-33
- **Docs:** account/organizations.md#delete-an-organization

### ACC-35 Get an Agent sandbox without an account
- **Surface:** CLI
- **Reach:** `curl -fsS -X POST <local>/api/agent/signup`; repeat 6 times within an hour from one address
- **Proof:** the reply has `token`, `organization`, `claimUrl` and `expiresAt` 24 hours out; `PLOYZ_TOKEN=<token> ployz status` acts in an Organization named Agent sandbox; the sixth call answers `429`; a deploy there asks nobody
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`--cloud`, `server add` with the sandbox token)
- **Compat:** none
- **Docs:** account/try-without-an-account.md

### ACC-36 Keep a sandbox with its claim link
- **Surface:** dashboard
- **Reach:** open `claimUrl` (`/claim/<secret>`) signed in → **Keep this Organization**; open it again; open a 25-hour-old one; open a bogus secret
- **Proof:** you become Owner on **Members** and its projects and servers are yours; the agent token keeps working (90 days); reopening reads **Someone already kept this sandbox**; old reads **This sandbox expired**; bogus reads **No sandbox at this link**; after a claim an agent's deploy asks a person
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/try-without-an-account.md#hand-over-the-claim-link

### ACC-37 Some things wait for a claim
- **Surface:** CLI
- **Reach:** with an unclaimed sandbox token: `ployz domain add web shop.example.com`, `ployz token new x`, `ployz org member invite a@b.c`, `ployz github connect`
- **Proof:** each stops with **Claim this Organization** and the claim link; a generated domain works
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** account/try-without-an-account.md#what-waits-for-a-claim

### ACC-38 An unclaimed sandbox ends after 24 hours
- **Surface:** daemon
- **Reach:** make a sandbox with a Server, let `expiresAt` pass (or move it back in Postgres), wait for the 15-minute sweep; repeat with the Server powered off
- **Proof:** projects then the Organization are removed and the Server leaves it; with the Server off, removal waits until an hour of silence, then forgets it; the forgotten Server's containers keep running (`docker ps`)
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server (`--cloud`, `power off`)
- **Compat:** Cloud→Server
- **Docs:** account/try-without-an-account.md#when-nobody-claims-it
