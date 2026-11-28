# Environments

Projects, Environments and Branches: creating, renaming and deleting Projects; empty Environments and Branches of one; what a Branch shares, copies and keeps secret; Sync between Environments and Production's changes following into a Branch; keeping, closing, shutting down and deleting; Protected Environments; an Environment's region; and PR Environments (previews). Service-level behavior inside an Environment is in [services.md](services.md); Deploy itself in [deploy.md](deploy.md).

Most entries are Config Store behavior, so a verify-dashboard run without Servers drives the panels (seed: project `shop` with `production`, Branches `fix-api` with api and web, kept `staging` with api and worker, and `search` below staging; Ada owner, Grace admin, Linus member via `scripts/verify/as.sh`). Handles are in `.agents/skills/verify-dashboard/features/branches.md`. Add `SERVERS=1` for anything that must run, remove or wait on a Server. CLI entries also work Standalone on `verify-cluster up`, where `ployz …` means `scripts/verify-cluster cli "$run" -- …`. PR Environments need `FAKE_GITHUB=1`; whether the fake GitHub can open and merge a pull request is unconfirmed, so a verifier that can't drive one reports those entries as not run.

### ENV-01 List the organization's projects
- **Surface:** CLI
- **Reach:** `ployz project ls`, `ployz project ls --json`
- **Proof:** one row per Project, `shop` among them; after `ployz project rm` finishes, the Project is gone from the list
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (CLI with `PLOYZ_TOKEN=$(jq -r .cliToken .verify/run/seed.json)`)
- **Compat:** CLI→Cloud: read; none expected
- **Docs:** **undocumented** (only --help; cli/overview.md names it in passing)

### ENV-02 Create a project
- **Surface:** dashboard
- **Reach:** `/cloud/ada/~` → **New project** (`/cloud/ada/new`) → pick **GitHub repository**, **Docker image** or **Database**; also CLI `ployz project new demo`; MCP `project_create`
- **Proof:** the Project opens on its `production` canvas, which shows the **Protected** lock; `ployz env ls --project demo` lists `production` as default and Protected; `ployz get @environment.region --project demo` shows the first Server's region (or none when any Server has no region); a taken name is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 for the region)
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** deploy/github.md#add-a-service-from-a-repository

### ENV-03 Rename a project
- **Surface:** CLI
- **Reach:** `ployz project rename shop store`; also dashboard `/cloud/ada/shop/production/settings` → **Project** → **Name**; MCP `project_rename`
- **Proof:** `ployz project ls` lists `store`; the running services keep their containers (`ployz ps` IDs unchanged) because their Namespaces stay; a name another Project has is refused
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 to show the containers stay)
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** **undocumented** (only --help; account/agent-sidebar.md says only that the agent can rename one)

### ENV-04 Delete a project
- **Surface:** CLI
- **Reach:** `ployz project rm shop` (answer the prompt, or `--confirm shop`); `--expect-version <V>`, `--accept-volume-loss pg-data`, `--detach`; also dashboard **Settings → Project → Delete this project** (type `shop`); MCP `project_remove`
- **Proof:** the command lists what goes (Branches before their Parents, the default last) and asks; with `--json` and no terminal it exits with `confirmation_required` and the exact retry; once done, `--json` reports `removal: removed` and `ployz project ls` drops it, and the Machines hold no containers or volumes of it; with a Machine powered off it exits 3 naming it and `removal: removing` lists `waiting_on`; a member gets **An owner or admin does this.**; from MCP, record whether it runs or comes back `needs a person` (account/organizations.md says an agent can't send that yes, account/agent-sidebar.md says the agent can remove a project: a docs conflict)
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=2); verify-removal-marks
- **Compat:** Cloud→Server: removal Deployments; a 0.3.19 Server's removal needs a cell
- **Docs:** environments/environments.md#delete-a-project

### ENV-05 Change the project's default environment
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/settings` → **Project** → **Default environment** → `staging`; also CLI `ployz env default staging`
- **Proof:** `/cloud/ada/shop` now redirects to `/cloud/ada/shop/staging`; `ployz env ls` marks staging default; commands without `--env` act on staging; production can now be deleted once unprotected
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#change-the-default-environment

### ENV-06 List a project's environments
- **Surface:** CLI
- **Reach:** `ployz env ls`, `ployz env ls --json`
- **Proof:** rows for production, fix-api, staging and search with Parent, default and Protected marks; a Branch closing reads `pending (synced)` or `pending (idle)` under REMOVAL, then the removal Deployment and its status like `#4 running`
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#keep-a-branch-or-let-it-close

### ENV-07 Start an empty environment
- **Surface:** dashboard
- **Reach:** Environment name in the top bar → **New environment** → Name `qa` → **Add environment**; also CLI `ployz env new qa`
- **Proof:** `qa`'s canvas is empty, with no services or variables, and `ployz env ls` lists it with no Parent; its region is the first Server's; asking the Agent to create an environment is declined
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#start-with-an-empty-environment

### ENV-08 Branch an environment, choosing what to copy
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/new-branch` (or **New branch of production**) → Name `staging2` → **Everything** or **Only what changes**, click rows → switch `Keep it after syncing into production` → **Create and deploy** or **Just create**; also CLI `ployz env branch staging2 --copy web --copy postgres --live redis --keep --from production`
- **Proof:** rows read **Separate**, **New, empty**, **production's** (amber with a warning for a shared service with data) or **Not included**; the Branch canvas shows copies as `link "web, New, Not deployed"` and shared ones as `production's redis`; the submit stays "Pick something to change" until a resource is included; after deploy, the copy's volume is empty (`ployz exec postgres --env staging2 -- psql -c '\dt'` lists none)
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 for the deploy)
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** environments/environments.md#create-staging

### ENV-09 Branch to fix a failed deployment
- **Surface:** CLI
- **Reach:** a failed Deployment `#N` in production; `ployz env branch fix-n --fix N`
- **Proof:** the new Branch holds copies of exactly what Deployment `#N` failed to apply, and `ployz diff --env fix-n` lists those changes
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** **undocumented** (only --help)

### ENV-10 A branch starts with its secrets Not set
- **Surface:** CLI
- **Reach:** `ployz env branch staging2 --copy api`; `ployz env copy api --env fix-api`; `ployz env keep --env staging2`; then dashboard `api` → **Variables** → **Set value**
- **Proof:** each command prints `1 secret not set in staging2: api.env.SECRET_KEY` with the command to set it; the card reads `1 not set`; a deploy before **Set value** leaves the variable out and says so; a generated value gets a new value in the copy
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1, after `seed-sealed.ts` seals SECRET_KEY)
- **Compat:** none
- **Docs:** environments/environments.md#create-staging

### ENV-11 A setup command seeds a new branch
- **Surface:** CLI
- **Reach:** `ployz env branch staging2 --copy web --copy postgres --setup 'web=sh -c "echo seeded > /tmp/s"'`; `ployz get setup.web --env staging2`, `ployz set setup.web='…' --env staging2`, `ployz unset setup.web --env staging2`; also the New branch panel **Add a command**
- **Proof:** the command runs before `web` first starts and again each deploy until `web` deploys successfully; `ployz set setup.web` takes effect at once and `ployz diff` shows it until then; a failing command fails the deploy and a fixed one passes the next; a service named `setup` is refused
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** environments/environments.md#create-staging

### ENV-12 Give every new branch the same setup commands
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/settings` → **Environment** → **Branch setup** → **Add command** (Command, service); also CLI `ployz env setup --env production --setup 'web=pnpm db:seed'`, `ployz env setup --env production --clear`
- **Proof:** a reload keeps the row; a Branch created after it with no `--setup` shows `ployz get setup.web --env <branch>` set to that command; with no services the section reads **Add a service first.**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#create-staging

### ENV-13 A branch gets its own generated address
- **Surface:** daemon
- **Reach:** production's `web` with a generated domain; branch `staging2 --copy web`, deploy it
- **Proof:** `ployz get web.domains --env staging2` shows `web-staging2.<cluster domain>`; `curl` on it answers from the Branch's copy while production's address answers from production; custom domains aren't copied
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a real DNS name for HTTPS (Hosted DNS is fake `*.ployz.test` in the lab)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** environments/environments.md#test-staging-at-its-own-address

### ENV-14 Make a shared service separate in a branch
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/fix-api` → click `production's postgres` (opens `live/<lineage>`) → **Make it separate** or **Give it a new, empty one**; also CLI `ployz env copy postgres --env fix-api`
- **Proof:** the live panel names whose service it is and who in the Branch uses it; after the copy the node reads `postgres, New, …`, and after deploy its volume starts empty; `--expect <stale revision>` is refused
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 for the deploy)
- **Compat:** none
- **Docs:** environments/environments.md#share-a-service-or-leave-it-out

### ENV-15 Sync a branch's changes into its parent
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/fix-api` → `button "Sync · 2"` → `dialog "Make production match fix-api"` → untick one → **Sync 1 change**; also CLI `ployz env sync --to --env fix-api --plan`, then `--version <V> --skip api.env.DEBUG --only web --close`
- **Proof:** production's canvas opens with the change staged and **Details** shows `region "Included"` (`fix-api · 1 change`); the unticked change is offered again next time; nothing deploys until **Deploy**; replicas, limits, domains and the Git branch don't sync; `--version` with a stale value is refused; with `Close fix-api after syncing` on, fix-api starts closing
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** CLI→Cloud: Config Store write
- **Docs:** environments/environments.md#sync-the-change-to-production

### ENV-16 Sync into any other environment
- **Surface:** dashboard
- **Reach:** Sync dialog → combobox `Place to sync into` → `staging`; also CLI `ployz env sync --to staging --env fix-api`, `ployz env sync --from fix-api --env staging`
- **Proof:** staging gets the changes staged; what fix-api only got from production is left out unless ticked; syncing from the default Environment into a Branch works too
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#sync-to-another-environment

### ENV-17 Secrets and generated values never sync
- **Surface:** CLI
- **Reach:** a sealed `TOKEN` added in fix-api; `ployz env sync --to --env fix-api`; then `printf '%s\n' "$PROD_TOKEN" | ployz env sync --to --env fix-api --value api.env.TOKEN`; also the dialog's secret row
- **Proof:** without `--value`, production gets `TOKEN` **Not set** (card `TOKEN not set`) and its deploy skips it and warns; with `--value`, it arrives sealed with that value; `--value` with `--approval` is refused; a generated variable arrives as `${{ secret(…) }}` and production makes its own value
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** environments/environments.md#sync-the-change-to-production

### ENV-18 Keep a setting out of every sync
- **Surface:** CLI
- **Reach:** `ployz env never-sync web.env.APP_ENV --env staging`, `--off` to undo; also **Never sync** in the Sync dialog or the variable's ⋮ menu
- **Proof:** a later `ployz env sync --to --env staging --plan` doesn't list `web.env.APP_ENV`; a Branch of staging still gets its value; after `--off` it's offered again
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#sync-the-change-to-production

### ENV-19 Take a sync back out
- **Surface:** dashboard
- **Reach:** after ENV-15, production **Details** → `Actions for fix-api` → **Remove**, **Include newer changes**; or the toast's **Undo**; also CLI `ployz env sync --undo <SYNC>`
- **Proof:** Remove gives production back what it had, keeping edits made there since; Undo after a second sync of the same Branch is refused; Remove is refused with a reason when production edited a service the sync brought or another included Environment changed it; **Save** or **Deploy** empties **Included**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#take-a-sync-back-out

### ENV-20 Production's deploys follow into a branch
- **Surface:** CLI
- **Reach:** production changes `web.env.X` and deploys; then `ployz diff --env staging`; where staging also changed it, `ployz env sync --take production --env staging --only web.env.X`; also **Details** → **Use theirs**
- **Proof:** staging's **Details** shows the change marked **From production's deploy** and nothing runs until staging deploys; where staging set the same row, staging's value stays and the parent's shows as a hint; `--take` stages it; a service production removes leaves staging only if staging never changed or referenced it
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** environments/environments.md#productions-changes-follow-into-staging

### ENV-21 Keep a branch, or close it
- **Surface:** dashboard
- **Reach:** Branch Sync **▾** (`button "More sync actions"`) → `Keep fix-api` / **Close fix-api…** (type `shop/fix-api`) → **Close branch**; also CLI `ployz env keep --env fix-api`, `--off`
- **Proof:** a closed Branch comes off the Machines and leaves `ployz env ls` once its removal Deployment runs; an unkept Branch idle 7 days closes at the next hourly sweep and shows `pending (idle)` until then; a kept Branch, one never deployed, one with Branches or the default never closes on its own
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1); verify-removal-marks
- **Compat:** Cloud→Server: removal Deployment
- **Docs:** environments/environments.md#keep-a-branch-or-let-it-close

### ENV-22 Protect an environment
- **Surface:** CLI
- **Reach:** `ployz env protect production`, `ployz env protect production --off` (asks yes or no; `--approval <DIGEST>` without a terminal); also dashboard **Settings → Environment → Protected**; MCP `environment_protect`
- **Proof:** `ployz env ls` marks it Protected and the header shows the lock; `ployz env rm production` is refused while Protected; protecting a Branch or PR Environment is refused; `--off` from MCP asks a human; as Linus the switch is disabled
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** environments/environments.md#protect-an-environment

### ENV-23 Members ask an owner or admin in a Protected environment
- **Surface:** dashboard
- **Reach:** `scripts/verify/as.sh linus`, open `/cloud/ada/shop/production` → **Request deploy**, inspector ⋮ → **Request restart** / **Request stop**; then as Grace, **Waiting on you** in the Agent panel → approve; also `ployz deploy` with `PLOYZ_TOKEN=$(jq -r .linusToken …)`
- **Proof:** Linus sees **Waiting on an owner or admin.** with a link; the CLI prints the link and waits; once Grace approves it runs as Linus; Linus's **Sync** and volume **Storage limit** are disabled, and `ployz env shutdown`, `volume move`, connecting a repository or changing the deploy branch refuse with **An owner or admin does this.**
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1 for the restart to run)
- **Compat:** none
- **Docs:** environments/environments.md#protect-an-environment

### ENV-24 A Deploy token in a Protected environment can't destroy
- **Surface:** CLI
- **Reach:** as Ada make a Deploy token for production (account area); with it, `ployz deploy` a change, then a staged `ployz service rm extra` and `ployz deploy`, and `ployz deployment redeploy <N>` of a removal; as Linus try to make one
- **Proof:** the plain deploy runs without asking; the removal is refused with `Needs changes:destroy: this deploy would remove Service extra. Have an owner or admin deploy it.`; a redeploy that removes again is refused with `An owner or admin does this.`; Linus sees `production is Protected; only an owner or admin can make a Deploy token for it.`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** environments/environments.md#protect-an-environment

### ENV-25 Set an environment's region
- **Surface:** CLI
- **Reach:** Machines labelled `region=fsn` and `region=syd`; `ployz env region fsn`, `ployz deploy --plan`, `ployz deploy`, `ployz env region --clear`, `ployz discard @environment.region`; also dashboard **Settings → Environment → Region → Runs in**; MCP `environment_region`
- **Proof:** `--plan` prints `web → fsn` per service; after deploy, `ployz ps --sort machine` shows replicas only on fsn Machines, and one already there keeps its container ID; `ployz env region lhr` is refused with `error: No Server is in lhr. Regions: fsn, syd.` and stages nothing; `ployz diff` shows `@environment.region`; **Runs in** lists `Falkenstein · …` and a serverless region disabled; a service with its own counts keeps them
- **Needs:** 3 Machines · Volumes: none · Cloud: no (Standalone); local Cloud for the dashboard
- **Drive:** verify-server; verify-region-placement
- **Compat:** CLI→Cloud: an older `ployz` deploys with the region but `ployz get` and `ployz diff` don't show it
- **Docs:** services/regions.md#set-an-environments-region

### ENV-26 A new environment runs in the first server's region
- **Surface:** CLI
- **Reach:** label every Machine with a region (first enrolled `region=fsn`); `ployz env new qa`; `ployz get @environment.region --env qa`; then remove one Machine's label and `ployz env new qa2`
- **Proof:** `qa` reads `fsn`; `qa2` has no region because a Server has none; a Branch reads its Parent's region and follows later changes; a sync doesn't copy the region
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/regions.md#a-new-environments-region

### ENV-27 Delete an environment
- **Surface:** CLI
- **Reach:** `ployz env rm staging` (prompt, or `--confirm shop/staging`), `--expect-version <V>`, `--accept-volume-loss <vol>`, `--detach`; also dashboard **Settings → Environment → Delete this environment** (type `shop/staging`); MCP `environment_remove`
- **Proof:** it says what goes, like `Removing shop/staging removes Service web.`, and asks; a stale `--expect-version` is refused naming the current one; once done, `ployz env ls` drops it and its containers and volumes are gone from the Machines; with a Machine off it exits 3 naming it and settings link to that Server; it's refused while it has Branches, is Protected or is the default; from MCP record `removed`/`removing` or `needs a person` (docs conflict, see ENV-04)
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=2); verify-removal-marks
- **Compat:** Cloud→Server: removal Deployment
- **Docs:** environments/environments.md#delete-an-environment

### ENV-28 Shut an environment down and bring it back
- **Surface:** CLI
- **Reach:** `ployz env shutdown pr-142` (`--accept-volume-loss`, `--approval`), then `ployz deploy --env pr-142`; also a preview's Sync **▾** → **Shut down until the next push** (type `shop/pr-142` when it has data)
- **Proof:** its containers and own volumes leave the Machines while `ployz env ls` still lists it with its settings; a push doesn't redeploy it; `ployz deploy --env pr-142` starts it fresh with empty databases and setup commands running again; as a member in a Protected Environment it's refused
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** Cloud→Server: removal Deployment
- **Docs:** environments/preview-environments.md#shut-down-a-preview

### ENV-29 Turn on PR environments for a repository
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production/settings` → **Project** → **PR environments** → repository (`pr-environments/<repositoryId>`) → **Enable PR environments**, **Start from**, **Services**, **Setup command**, **Delete when the PR closes**, **Enable bot PR environments**; also CLI `ployz env pr acme/web --on --from production --copy postgres --setup 'web=pnpm db:seed' --remove-on-close true --bots false`; MCP `environment_pr`
- **Proof:** the panel saves as it goes and a reload keeps each switch; `ployz env pr` shows the plan; an uncopied database shows **production's** in amber; with no GitHub-deployed service the section is absent; without the app's permissions the repository reads **Needs permissions approved on GitHub**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: GitHub (FAKE_GITHUB=1 covers the connection)
- **Drive:** verify-dashboard (FAKE_GITHUB=1)
- **Compat:** none
- **Docs:** environments/preview-environments.md#turn-on-preview-environments

### ENV-30 Approve an outside pull request's preview
- **Surface:** CLI
- **Reach:** a pull request from a fork or a non-member; `ployz env pr` then `ployz env pr approve 142 --repository acme/web`; also **Approve deploy** under **Open now**
- **Proof:** `ployz env pr` lists `#142 needs approval a1b2c3d` and a member's PR as `#140 deploys`; approval covers that commit only, and a new push reads **Needs approval** again; approving after a newer push is refused with **This pull request has a newer commit. Approve that one.**; with no Servers the PR stays under **Open now**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: GitHub pull requests from two accounts (FAKE_GITHUB=1 if it can open them)
- **Drive:** verify-dashboard (FAKE_GITHUB=1)
- **Compat:** none
- **Docs:** environments/preview-environments.md#who-deploys-automatically

### ENV-31 A preview's build is refused in this version
- **Surface:** daemon
- **Reach:** PR Environments on (ENV-29) with Servers; open a member's pull request
- **Proof:** the preview `pr-142` exists with its settings and check, but its Deployment fails at the build with `This server runs only builds an approved device starts.`; an image-only preview fails like a dashboard deploy with no passkey
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: GitHub (FAKE_GITHUB=1 if it can open a pull request)
- **Drive:** verify-dashboard (SERVERS=1, FAKE_GITHUB=1)
- **Compat:** Cloud→Server: Servers refuse unsigned builds Cloud starts
- **Docs:** environments/preview-environments.md#previews-dont-deploy-in-this-version

### ENV-32 What each pull request's preview gets
- **Surface:** dashboard
- **Reach:** open `/cloud/ada/shop/pr-142` after a pull request opens; environment switcher under production
- **Proof:** the preview is a Branch named `pr-142` listed under its start-from Environment and **Open now**; every service plans one replica (in the region with the most for per-region counts); a generated address reads `web-pr-142.<cluster domain>`; custom domains aren't copied; sealed variables read **Not set**; a copied database refuses the automatic deploy until a dashboard Deploy makes its password
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud · External: GitHub (FAKE_GITHUB=1 if it can open a pull request)
- **Drive:** verify-dashboard (SERVERS=1, FAKE_GITHUB=1)
- **Compat:** none
- **Docs:** environments/preview-environments.md#what-each-pull-request-gets

### ENV-35 Show an environment to an agent
- **Surface:** MCP
- **Reach:** `ployz mcp` (stdio), remote `/mcp`, or the dashboard Agent sidebar: `environment_show` with `{"environment":"staging"}`
- **Proof:** returns staging's services with source and domains, volumes, configs, the staged-change count, broken references, the latest Deployment and what waits, the revision, and every Project with its Environments
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard
- **Compat:** none
- **Docs:** **undocumented** (account/agent-sidebar.md names **ployz environment show** only as a card title)

### ENV-36 Check what an environment still needs before it can deploy
- **Surface:** MCP
- **Reach:** `environment_setup` from `ployz mcp` or the Agent sidebar, in an Organization with no Servers, then with one, then with a repository service and no GitHub connection
- **Proof:** returns not ready with a dashboard page for each missing item (Servers, a Cluster Domain, GitHub), then ready once each is in place
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: GitHub (FAKE_GITHUB=1 covers it)
- **Drive:** verify-dashboard (SERVERS=1)
- **Compat:** none
- **Docs:** **undocumented**
