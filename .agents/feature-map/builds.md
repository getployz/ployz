# Builds

How Ployz turns a Git service or an upload into an image: Railpack and Dockerfile builds, build-time variables and sealed values, the network fence around a build, where builds run (your Servers or GitHub Actions), the build order and Preferred Builder, which builds need approval, per-Server build limits, the build cache and its cleanup, and the GitHub connection that feeds all of it. Deploy-on-push, Wait for CI and the automatic deploy rule are in [deploy.md](deploy.md). Verifiers start with `ployz up` on a Standalone Boat Cluster (BLD-01, BLD-09), which always builds on a Server, then a local Cloud with `FAKE_GITHUB=1` for the GitHub entries. The lab has no ARM Machine and no real GitHub Actions runner: BLD-07 and a real Actions build in BLD-18 need them from outside.

### BLD-01 Build an app with Railpack, no Dockerfile
- **Surface:** CLI
- **Reach:** `ployz up` in a Node (or Python, Go) app's Git checkout; dashboard: a GitHub service with **Settings → Build → Build method** Railpack (default)
- **Proof:** the build log's first line reads `Building on machine-1`; the service answers on its https address; a repository Railpack can't read fails with the reason from troubleshooting ("Railpack can't tell how to build my app")
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon
- **Docs:** builds/railpack-and-dockerfiles.md#build-without-a-dockerfile

### BLD-02 Change the build and start commands
- **Surface:** dashboard
- **Reach:** **Settings → Build → Build command** `pnpm run build`; **Settings → Deploy → Start command** `node server.js` (CLI `ployz set web.buildCommand=… web.startCommand=…`); a `railpack.json` in the root directory
- **Proof:** the build log shows the custom build step; `docker inspect` on the Server shows the start command; changes stage until deploy
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** builds/railpack-and-dockerfiles.md#change-the-build-and-start-commands

### BLD-03 Build with your own Dockerfile
- **Surface:** dashboard
- **Reach:** **Settings → Build → Build method** Dockerfile, **Dockerfile path** `docker/web.Dockerfile` (CLI `ployz set web.dockerfilePath=…`)
- **Proof:** the build log shows the Dockerfile's steps; a wrong path fails per troubleshooting "Ployz can't find my root directory or Dockerfile"; a **Start command** replaces `ENTRYPOINT`/`CMD` without a shell
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** builds/railpack-and-dockerfiles.md#build-with-a-dockerfile

### BLD-04 Variables reach the build
- **Surface:** CLI
- **Reach:** `ployz set web.env.NEXT_PUBLIC_API_URL=https://x` and a Dockerfile with `ARG NEXT_PUBLIC_API_URL` and `RUN echo $NEXT_PUBLIC_API_URL > /built`; `ployz deploy`
- **Proof:** `ployz exec web -- cat /built` prints the value; a Railpack build sees it without `ARG`; changing the variable rebuilds (no `build reused`), changing replicas reuses (`build reused`)
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/railpack-and-dockerfiles.md#use-variables-at-build-time

### BLD-05 A build reads sealed values
- **Surface:** CLI
- **Reach:** `ployz set web.env.NPM_TOKEN --secret`; Dockerfile `RUN --mount=type=secret,id=NPM_TOKEN,env=NPM_TOKEN sh -c 'test -n "$NPM_TOKEN"'`; then a sealed `RAILPACK_BUILD_CMD`; deploy from an approved device
- **Proof:** the secret-mount build passes and `docker history` on the Server doesn't show the value; the build log masks it; a Dockerfile without the secret mount gets only `ARG`-declared sealed values; a sealed `RAILPACK_*` is refused on Servers; a Server older than 0.3.5 refuses naming the sealed values
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** CLI→daemon: Servers open sealed values for builds only from 0.3.5
- **Docs:** builds/railpack-and-dockerfiles.md#read-a-sealed-value

### BLD-06 A build can't reach the private network or metadata
- **Surface:** daemon
- **Reach:** Dockerfile steps `RUN curl -m5 https://example.com`, `RUN curl -m5 http://<a service's private address>`, `RUN curl -m5 http://169.254.169.254`, `RUN curl -m5 http://<the build Server's own address>`
- **Proof:** the internet fetch succeeds; the private address, the metadata address and the Server itself time out or are refused; DNS (port 53) still resolves; a `FROM` from a registry on a private address fails; if the fence can't be installed the build fails instead of running
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** upgrade: the container network fence is new in 0.4 (iptables chain, `br_netfilter`)
- **Docs:** builds/railpack-and-dockerfiles.md#what-a-build-can-reach

### BLD-07 Servers that mix x86 and ARM
- **Surface:** daemon
- **Reach:** a Cluster with one x86 and one ARM Server; deploy a Railpack service with replicas on both, then a Dockerfile service
- **Proof:** the Railpack image runs on both (`docker image inspect` shows both platforms); the Dockerfile deploy fails before changing anything with `Build for Service web contains linux/amd64; destination Machine … reports architecture aarch64`; GitHub skips with `GitHub builds one platform, and this Service runs on linux/amd64 and linux/arm64`
- **Needs:** 2 Machines · Volumes: none · Cloud: no (Standalone) · External: an ARM Machine (Boat is x86 only)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/railpack-and-dockerfiles.md#when-your-servers-mix-x86-and-arm

### BLD-08 Ployz reuses an image when nothing the build reads changed
- **Surface:** CLI
- **Reach:** `ployz deploy` twice with no change; then `ployz deployment redeploy N`; then change `web.replicas`
- **Proof:** the second deploy's build row reads `build reused` (`--json` build `"status": "reused"`); a redeploy of the same commit reuses; a variable change rebuilds; a failed build leaves the old version running and later deploys reuse the images that did build
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/overview.md#when-ployz-builds

### BLD-09 Read the build log
- **Surface:** dashboard
- **Reach:** the deployment's page → the service's build card; CLI `ployz logs --deployment N --build web`
- **Proof:** the first line names `Building on machine-1` or `Building on GitHub Actions:` with a run link; a failing step ends in a line starting `ERROR`; the header reads `Build failed. web: npm run build: … exit code: 1`; the CLI prints the same lines on stdout
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1; verify-server for the CLI
- **Compat:** CLI→Cloud
- **Docs:** builds/overview.md#read-the-build-log

### BLD-10 Choose which Servers run builds and how many at once
- **Surface:** dashboard
- **Reach:** **Servers → machine-1 → Builds**: **Run builds here**, **Builds at once**; CLI `ployz server set machine-1 --accepts-builds false` and `--build-concurrency 1`
- **Proof:** with builds off everywhere a deploy fails with `No eligible Build Machine was selected; no build was started.` and `Server machine-1 (…): builds are turned off`; with concurrency 1 a second build logs `Waiting for a build slot`; a repeat build lands on the Server that built last
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2; verify-server for the CLI flags
- **Compat:** CLI→daemon
- **Docs:** builds/where-builds-run.md#build-on-your-servers

### BLD-11 Give builds more time, or time out a waiting build
- **Surface:** daemon
- **Reach:** a build with `RUN sleep 1900`; then `echo 'PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS=3600' >> /etc/default/ployz && systemctl restart ployz`; also `PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS`, and a value of 90000
- **Proof:** the default build fails with `the build exceeded its 1800s execution timeout and was terminated`; with 3600 it passes; above 86400 `systemctl status ployz` shows the daemon refusing to start; the queue timeout fails a build waiting for a slot
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/build-resources-and-cache.md#give-builds-more-time

### BLD-12 Cap a build's CPU, memory and cache with build.yaml
- **Surface:** daemon
- **Reach:** on the Server as root, `/root/.ployz/build.yaml` with `memory_bytes: 268435456`; then a typo such as `cpu_cores: two`
- **Proof:** a memory-hungry build step fails with exit code 137; the typo fails every build with `invalid host build.yaml: …`; no restart is needed for either; with `cache_bytes` set the cache settles near half of it per slot cache
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/build-resources-and-cache.md#cap-cpu-memory-and-cache

### BLD-13 Clear the build cache
- **Surface:** CLI
- **Reach:** on the build Server as root, `ployz server build-cache-clear`; again while a build runs
- **Proof:** build-cache disk use drops (`docker buildx du` or `du` under the builder's state) while `docker image ls` keeps every image; during a build it refuses; it needs no daemon (works with `systemctl stop ployz`)
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/build-resources-and-cache.md#free-up-disk-space

### BLD-14 Old built images and unused free-build images are cleaned up
- **Surface:** daemon
- **Reach:** deploy a Git or upload service several times; deploy a service that reaches no data, then fill the Docker disk to nearly full and build again
- **Proof:** after each deploy only the images in use remain among those Ployz built (`docker image ls`), while pulled images stay; unnamed free-build images unused for over an hour are removed when the disk is nearly full; without `cache_bytes`/`min_free_bytes` a fifth of the Docker disk stays free
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** builds/build-resources-and-cache.md#free-up-disk-space

### BLD-15 A builder stays quarantined while an unconfirmed build still runs
- **Surface:** daemon
- **Reach:** start a long build on machine-1, `kill -9` the build's `ployz` process so it never confirms its end while its BuildKit step keeps running, then deploy again; then let the step end; also `systemctl restart ployz` and `ployz server build-cache-clear` in between
- **Proof:** the next build fails with `builder ployz-… stays quarantined: the Build that last used it never confirmed its end, and still running: …` naming PIDs; `build-cache-clear` refuses the same way; once the processes end the next build takes the builder back with no action
- **Needs:** 1 Machine · Volumes: none · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** troubleshooting/builds.md#a-build-says-its-builder-stays-quarantined

### BLD-16 Builds of services that reach no data need no approval
- **Surface:** daemon
- **Reach:** on enforcing 0.4 Servers, dashboard **Deploy** (unsigned) of a Git service with no volume, sealed value or login; then add a sealed variable and deploy unsigned again; then a Cluster with one beta.2 Server
- **Proof:** the free build runs on a Server whatever the build order says and the service deploys; the data-reaching build fails with `No eligible Build Machine was selected; no build was started: Server web-1 (…): This server runs only builds an approved device starts.`; with the old Server it stops with `web-2 can't run web, which builds without approval. Upgrade web-2, then deploy again.`; once signed, the service keeps needing a signature
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=2 FAKE_GITHUB=1; verify-approval-binding
- **Compat:** Cloud→Server: Servers before this release can't run a build without approval and are named
- **Docs:** builds/where-builds-run.md#which-builds-need-an-approved-device-or-a-rule

### BLD-17 Add the GitHub Actions build workflow to a repository
- **Surface:** dashboard
- **Reach:** **Organization → Builds** (`/cloud/ada/~/settings?section=builds`) → **GitHub Actions** → **Add workflow ↗** / **Update workflow**
- **Proof:** GitHub opens the prefilled `.github/workflows/ployz-build.yml`; the row reads **Waiting for commit**, then a check mark once the fixture holds the file; a different file reads **Uses an older workflow**; missing Actions permission reads **Installation lacks permission**
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard FAKE_GITHUB=1
- **Compat:** none
- **Docs:** builds/where-builds-run.md#build-on-github-actions

### BLD-18 Build on GitHub Actions and push to a Server
- **Surface:** daemon
- **Reach:** a repository with the workflow and build order **GitHub, then your servers**; deploy; the run executes `ployz build --commit <sha> --fingerprint <fp>` with `PLOYZ_BUILD_GRANT`
- **Proof:** the build log reads `Building on GitHub Actions:` with the run link; the image lands on the granted Server and deploys; sealed values reach the runner still sealed and masked in the log; a run past 2 hours or ending without an image hands the build to the next builder; a grant Server now in another Cluster fails with `Machine … belongs to another Cluster`; disconnecting the account mid-build fails with "GitHub account `<login>` was disconnected"
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: a real GitHub repository and Actions runner for a real image (FAKE_GITHUB=1 covers dispatch, OIDC and run states)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1; verify-build-handoff
- **Compat:** Cloud→Server: `GitHub builds need a newer Ployz on web-1` for an old grant Server
- **Docs:** builds/where-builds-run.md#build-on-github-actions; `ployz build` itself **undocumented** (only --help)

### BLD-19 GitHub skips a build and the next builder takes it
- **Surface:** dashboard
- **Reach:** with **GitHub, then your servers**, deploy a service whose repository lacks the workflow (fixture without the file), or is public without the app, or was deployed with `ployz up`; then set **GitHub only**
- **Proof:** the build log reads `GitHub skipped: acme/web has no .github/workflows/ployz-build.yml on its default branch` then `Building on machine-1`; with **GitHub only** the deployment fails with `…No other Builder in your Build Order can take it`; an upload still builds on a Server
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1; verify-build-handoff
- **Compat:** none
- **Docs:** troubleshooting/builds.md#the-build-log-says-github-skipped

### BLD-20 Choose the Organization's build order
- **Surface:** dashboard
- **Reach:** **Organization → Builds → Where builds run**; CLI `ployz org build-order` (show) and `ployz org build-order servers-only` (values `auto`, `servers-only`, `github-then-servers`, `servers-then-github`, `github-only`)
- **Proof:** the next build follows the order (build log first line); a member sees the control disabled with the reason, and the CLI as a member is refused (`Needs org:settings`); `ployz org build-order` prints the value set in the dashboard
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** CLI→Cloud
- **Docs:** builds/where-builds-run.md#choose-the-build-order; `ployz org build-order` itself **undocumented** (only --help)

### BLD-21 Prefer a builder for one service
- **Surface:** CLI
- **Reach:** `ployz set web.preferredBuilder=machine-1`; `ployz set web --patch '{"preferredBuilder":"github"}'`; a name two Servers share (dashboard **Settings → Build → Preferred Builder**)
- **Proof:** `ployz get web.preferredBuilder` shows machine-1's ID; the next build logs `Building on machine-1` with no deploy in between (applies at once); a shared name is refused listing both IDs; with **GitHub only** a service preferring a Server still builds on Servers
- **Needs:** 2 Machines · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** none
- **Docs:** builds/where-builds-run.md#prefer-a-builder-for-one-service

### BLD-22 Source size, submodule and LFS refusals
- **Surface:** CLI
- **Reach:** deploy a repository (or `ployz up` a directory) with a Git submodule; one with LFS content; one over 100,000 files
- **Proof:** fails with `Git submodules are not supported for Cloud builds.`, `Git LFS content is not supported for Cloud builds.` or `Source archive has too many or duplicate entries.`; nothing changes on the Servers
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1 fixture)
- **Drive:** verify-dashboard SERVERS=1 FAKE_GITHUB=1
- **Compat:** none
- **Docs:** troubleshooting/builds.md#my-repository-is-too-big-or-uses-submodules

### BLD-23 Each uploaded service keeps its latest upload
- **Surface:** CLI
- **Reach:** `ployz up` in `api/` and in `web/` separately; change a variable both builds read; `ployz deploy`
- **Proof:** a deploy with no new upload rebuilds from each service's last upload; deploying both refuses with `web must build again from an earlier upload than this Deployment's, … Deploy it on its own: ployz deploy web`; a queued deploy shows `uploaded by …, sha256 … for now; chosen again when it starts`
- **Needs:** 1 Machine · Volumes: none · Cloud: local Cloud
- **Drive:** verify-server --cloud
- **Compat:** none
- **Docs:** troubleshooting/deployments.md#deploy-says-a-service-must-build-again-from-an-earlier-upload

### BLD-24 Connect a GitHub account to the Organization
- **Surface:** dashboard
- **Reach:** repository picker **Connect GitHub** or **Organization → GitHub** (`/cloud/ada/~/settings?section=github`); **Use existing**; **Install on another GitHub account**; **Refresh repositories**
- **Proof:** **Organization → GitHub** lists the account with its repositories, who connected it and when ("Repositories appear in a moment" first); a member sees the list without Connect; connecting an account you don't own (fake user mallory) is refused with "Not an owner"
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard FAKE_GITHUB=1
- **Compat:** none
- **Docs:** deploy/github.md#who-owns-the-connection

### BLD-25 Connect GitHub from the CLI or an agent
- **Surface:** CLI
- **Reach:** `ployz github connect` (`--json` prints the link at once, `--wait` waits); MCP `github_connect`
- **Proof:** prints the connect link and returns once the account shows in `ployz github ls`; `github_connect` returns whether GitHub is connected, the readable repositories and the connect page
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard FAKE_GITHUB=1 (run the CLI against its Cloud)
- **Compat:** CLI→Cloud
- **Docs:** **undocumented** (only --help; account/agent-sidebar.md names the agent's **github connect**)

### BLD-26 List and disconnect GitHub accounts from the CLI
- **Surface:** CLI
- **Reach:** `ployz github ls`; `ployz github ls acme/blog`; `ployz github disconnect acme`
- **Proof:** `ls` lists accounts, repositories and the link to connect another; with a repository it lists branches; after disconnect the account is gone from `ls` and from **Organization → GitHub** of this Organization only, a build in flight fails with "GitHub account `acme` was disconnected", and the app stays installed
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard FAKE_GITHUB=1
- **Compat:** CLI→Cloud
- **Docs:** deploy/github.md#who-owns-the-connection

### BLD-27 Read a repository's files from the CLI or an agent
- **Surface:** CLI
- **Reach:** `ployz github tree acme/blog`; `ployz github tree acme/blog apps/web --ref dev`; `ployz github tree acme/blog --match '**/Dockerfile'`; `ployz github cat acme/blog package.json`; MCP `github_ls`
- **Proof:** tree lists paths, stopping at 500 with a note; a missing path fails with `No path` and exit 1; `cat` prints text up to 64 KiB, says so for a larger or binary file, and `No file` with exit 1 when missing; `github_ls` on a Dockerfile returns its exposed ports and ENV/ARG names
- **Needs:** 0 Machines · Volumes: none · Cloud: local Cloud · External: none (FAKE_GITHUB=1)
- **Drive:** verify-dashboard FAKE_GITHUB=1
- **Compat:** CLI→Cloud
- **Docs:** deploy/github.md#read-a-repository-from-the-cli
