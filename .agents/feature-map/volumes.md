# Volumes (VOL)

Data that outlives containers: Managed Volumes on each Server's ZFS pool (and plain Docker volumes), their size limit and resize, the one-writer rule, Copies (Mirrors and the old copies Moves leave), Volume Runs (Move, Mirror, sync, Release, Restore, resize) through Ployz Cloud, the Standalone restore, deletion into each Server's Trash, copies Ployz keeps because they may hold unique writes, leftovers, kept anonymous volumes and stray Namespaces. Verifiers start at `docs/user/services/volumes.md`, then `move-a-volume.md`, `mirror-a-volume.md`, `restore-a-volume.md`, and `troubleshooting/volumes.md` for the refusals to provoke. Every run-based feature (move, mirror, sync, release, Cloud restore, resize, leftovers) needs a local Cloud: `verify-cluster up --cloud` or verify-dashboard `SERVERS=N`. Boat Machines run ZFS, so Managed Volumes work there; prove data with `ployz exec <svc> -- md5sum <file>` and `zfs list -o name,quota,ployz:seed` on the Server. 0.3.19's CLI has no `volume leftovers`, `volume trash`, `server volumes` or `server clean`.

### VOL-01 Add a Volume in the dashboard and mount it on a Service
- **Surface:** dashboard
- **Reach:** `/cloud/ada/shop/production`, **Create** → **Volume**, Name `uploads`, **Storage limit (GB)** `5`, **Create volume**; click it → **Mount on a service**, pick `web`, Directory `/app/uploads`, **Mount**; **Deploy** (handles in `.agents/skills/verify-dashboard/features/canvas-and-inspector.md`)
- **Proof:** the Volume shows as a tray under `web`'s card; after deploy `ployz exec web -- sh -c 'echo hi > /app/uploads/f'`, redeploy, `cat` still prints `hi`; `zfs list` on the Server shows the dataset with a 5 GB quota; `Uploads Data!` saves as `uploads-data`
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Cloud→Server: a Volume first deployed on 0.4 needs 0.4 on every Server that holds it or runs its Service ("Upgrade web-2 first. files mounts a volume that needs a newer Ployz on every server it runs on.")
- **Docs:** services/volumes.md#add-a-volume

### VOL-02 Add a Volume from the CLI
- **Surface:** CLI
- **Reach:** `ployz volume add uploads --size 5 --mount web:/app/uploads`; `ployz volume add cache --docker`; `ployz volume add x --mount web` (bad form). Also: MCP `volume_add`.
- **Proof:** `ployz volume ls` shows `uploads` with `DEPLOYED no` and its next-deploy action; `ployz diff` lists it; `--mount web` is refused with `Expected --mount SERVICE:/PATH, like web:/var/lib/data`; a taken name fails
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19 has the same `volume add`
- **Docs:** **undocumented** (only --help)

### VOL-03 Ployz refuses a mount that breaks the mount rules
- **Surface:** dashboard
- **Reach:** mount a Volume on `web` at `/data` where a Config already mounts `/data/`; mount at `/proc/x`; discard or sync a change that would break one
- **Proof:** each is refused naming the clash; the form keeps the typed directory after a failed write; the refusal holds for discard, a Sync and a restore from history too; a mount saved before the rules existed doesn't block other changes
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/volumes.md#add-a-volume

### VOL-04 List an Environment's Volumes and what the next deploy does
- **Surface:** CLI
- **Reach:** `ployz volume ls`, `ployz volume ls --json`
- **Proof:** columns `VOLUME STORAGE SHARED WRITES MOUNTS DEPLOYED NEXT DEPLOY`; a staged removal shows in `NEXT DEPLOY`
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→Cloud: 0.3.19 has `volume ls`; compare its rows against main Cloud
- **Docs:** services/volumes.md#add-a-volume

### VOL-05 Inspect one Volume
- **Surface:** CLI
- **Reach:** `ployz volume inspect uploads` (`--json`). Also: MCP `volume_show` (where each copy lives and recent runs).
- **Proof:** fields `volume`, `storage`, `storage settings` (`editable before deployment`, or `deployed; ployz volume set --size resizes it (Ployz Cloud)`), `shared writes`, `deployed yes|no`, and the mounts
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone); local Cloud for `volume_show`
- **Drive:** verify-server
- **Compat:** none
- **Docs:** **undocumented** (only --help)

### VOL-06 Rename a Volume
- **Surface:** CLI
- **Reach:** `ployz volume rename uploads files`, then `ployz deploy`. Also: MCP `volume_update` with a name.
- **Proof:** `ployz diff` shows the rename staged; after deploy `ployz volume ls` lists `files` with the same mounts and `ployz exec web -- cat /app/uploads/f` still prints the old content
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** **undocumented** (only --help)

### VOL-07 Set the storage limit before the first deploy
- **Surface:** CLI
- **Reach:** `ployz volume set data --size 10`; `--size 0.05`; a deploy with a limit larger than any Server's free space; a Server whose trash holds the room. Also: dashboard Settings → **Space** → **Storage limit**.
- **Proof:** `volume inspect` shows `editable before deployment` and the new size; `--size 0.05` is refused (under 0.1 GB); the oversized deploy fails naming no Server with room, and the trash case says how much the trash holds, until when, and how to empty it; once a deploy starts creating it the dashboard field is fixed, and the CLI refuses for 10 minutes after a deploy ends
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/volumes.md#choose-the-storage-limit

### VOL-08 Grow a deployed Volume while its Service runs
- **Surface:** CLI
- **Reach:** `ployz volume set data --size 20 --wait` signed in to Cloud. Also: dashboard Settings → **Space**, **Storage limit** → **Grow**; MCP `volume_update` with a size.
- **Proof:** `zfs get quota` on every copy reads 20G, mirrors first; `ployz volume runs data` shows the resize done; `ployz diff` shows nothing and the container's ID is unchanged; a Server with no room fails the whole resize by name; Standalone (`--connect`) it refuses and says a deployed Volume keeps its limit
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`); verify-volume-runs for resize against moves and restarts
- **Compat:** Cloud→Server: a Server whose Ployz is too old to grow is refused by name before confirm
- **Docs:** services/volumes.md#choose-the-storage-limit

### VOL-09 Shrink a deployed Volume
- **Surface:** CLI
- **Reach:** write 3 GB, then `ployz volume set data --size 3 --wait`, `--size 8 --wait`; run the same size again to finish an interrupted shrink
- **Proof:** the 3 GB shrink is refused naming the Server and the smallest size it accepts (rounded up, holding 10% or 1 GB headroom); the 8 GB shrink lands writer first then the mirror (`zfs get quota`); a shrink during a deploy names that deployment and changes nothing
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-volume-runs
- **Compat:** Cloud→Server: a Server too old to shrink is named
- **Docs:** services/volumes.md#choose-the-storage-limit

### VOL-10 Turn a Volume's shared writes off
- **Surface:** CLI
- **Reach:** `ployz volume set uploads --shared-writes=false`; deploy a Volume that still has it on; redeploy a version saved with it on. Also: dashboard Settings → **Space** → **Allow shared writes**.
- **Proof:** it applies at once (`volume inspect` `shared writes no`); the deploy with it on is refused naming the Volume; with 2 replicas mounting it the switch-off is refused until the Service is at 1; the old version's redeploy is refused even after
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/volumes.md#share-a-volume

### VOL-11 One container writes to a Managed Volume
- **Surface:** daemon
- **Reach:** with `web` on `data`, start a second container on it (`docker run -v data:/x` on the Server, or deploy `worker` mounting it); then one deploy that unmounts it from `web` and mounts it on `worker`; then one deploy that swaps two Services' Volumes
- **Proof:** the second start fails naming `web` and its mount path; the handoff deploy starts `worker` only after `web`'s container is gone; the swap is refused
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: pre-upgrade containers need one signed deploy before an Operator start is admitted
- **Docs:** services/volumes.md#share-a-volume

### VOL-12 Use a plain Docker volume when no Server has Managed volumes
- **Surface:** dashboard
- **Reach:** a Cluster added with `--storage none`; deploy a Volume (fails); Settings → **Space** → **Plain Docker volume**, or **Create** → **Volume** → **Advanced** → **Use a plain Docker volume (not recommended)**. Also: `ployz volume set data --docker`, `ployz volume add data --docker`.
- **Proof:** the Servers page reads **Docker only**; the Managed deploy fails, including a database's; with the Docker kind it deploys and `docker volume ls` shows it with no quota
- **Needs:** 1 Machine · Volumes: Docker · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/volumes.md#when-your-servers-show-docker-only

### VOL-13 Put Managed volumes on an empty spare disk
- **Surface:** install
- **Reach:** `ployz server add … --storage zfs:/dev/<disk>` on a Machine with a blank second disk; stop the installer between steps and rerun; attach a second pool
- **Proof:** `zpool list` shows the Machine Pool on that disk and the Servers page reads **Managed volumes available**; a rerun after a stop adopts the same pool rather than making a second; the first Volume lands on it
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone) · External: a Boat Machine with a spare disk (check the lab offers one)
- **Drive:** verify-server (`verify-cluster up --bare`); verify-pool-ownership for the installer against the volume plugin's first Pool
- **Compat:** upgrade: a Server installed on 0.3.19 keeps its pool across the upgrade
- **Docs:** servers/add-a-server.md#use-an-empty-spare-disk-for-volumes

### VOL-14 Detach a Volume from a Service
- **Surface:** dashboard
- **Reach:** Volume panel, menu at the end of the mount's row → **Unmount**, Deploy; **Edit directory** to move it
- **Proof:** the detached row shows what the next deploy removes; after deploy the path is gone from `ployz exec web -- ls /app` and `zfs list` still shows the dataset
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** none
- **Docs:** services/volumes.md#detach-a-volume

### VOL-15 Delete a Volume and confirm its data goes
- **Surface:** dashboard
- **Reach:** Volume → **Delete volume** under **Danger** (**Keep volume** undoes), **Delete data**, type `shop/production`, **Deploy**. Also: `ployz volume rm data` then `ployz deploy` (asks to accept by name); MCP `volume_remove`.
- **Proof:** the sheet shows **Deletes data**; after deploy `ployz volume ls` lacks it and the dataset moved to the Server's trash (`sudo ployz volume trash ls`); with the Volume busy the deployment says `uploads: deleting its data failed: it is still in use on Server web-1` and the next deploy deletes without asking; a never-deployed Volume goes without asking
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=1
- **Compat:** Server upgrade: a Server without the trash deletes at once
- **Docs:** services/volumes.md#delete-a-volume

### VOL-16 Deleted data waits in the Server's trash for 7 days
- **Surface:** daemon
- **Reach:** `ployz volume rm data`, then `ployz deploy --accept-volume-loss data --expect-version <v>` (the refusal names `<v>`); restart `ployzd`; power the Server off for a while; advance past 7 days of uptime
- **Proof:** `zfs list` shows the dataset moved from `ployz/ployz/<project>-<env>_vol-<id>` to `ployz/ployz-trash/<entry id>`; `sudo ployz volume trash ls` lists the entry with DELETED and EXPIRES 7 days later, which moves only while the Server runs; `zfs list` still counts its space; after 7 days of uptime the entry and dataset are gone
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server upgrade: the sweep must survive a 0.4 daemon restart; 0.3.19 deletes at once
- **Docs:** services/volumes.md#delete-a-volume

### VOL-17 List a Server's trash
- **Surface:** CLI
- **Reach:** `sudo ployz volume trash ls` on the Server, or `ployz volume trash ls --connect ssh://<user>@<host>`; a context reaching several Servers
- **Proof:** each entry names `<project>-<env>_vol-<id>` and when it is deleted; a restoring entry reads restoring; a multi-Server context is refused and asks for `--connect`; an unreadable entry prints its delete command and exits 3
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.3.19's CLI has no `volume trash`
- **Docs:** services/volumes.md#get-deleted-data-back

### VOL-18 Get deleted data back from the trash
- **Surface:** CLI
- **Reach:** `sudo ployz volume trash restore <ID>`; then `ployz deployment redeploy <n> --project shop --env production`, `ployz discard volumes.data …`, `ployz deploy --skip-deploy …`
- **Proof:** restore prints `Restored <namespace>_vol-<id> on machine-1. No service mounts it yet.`, the entry leaves `trash ls`, the dataset returns under `ployz/ployz/` unmounted, and after `ployz deployment redeploy <n>` of a deployment from before the removal `ployz exec web -- cat /data/<file>` prints the old content; with a second Server on an older Ployz or offline the entry stays restoring and each try adds 7 days; rerunning after a lost answer finishes it; a mirror's data comes back as an old copy
- **Needs:** 1 Machine for the restore; 2 for the waiting cases · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** Server↔Server: the restore waits on a Server running an older Ployz
- **Docs:** services/volumes.md#get-deleted-data-back

### VOL-19 Empty a Server's trash
- **Surface:** CLI
- **Reach:** `sudo ployz volume trash empty`, `… empty <ID>`, `… empty --keep-restoring`; `… empty </dev/null` (no terminal)
- **Proof:** it asks first and then `zfs list` lacks the datasets; restoring entries stay unless named; `--keep-restoring` skips them; with no terminal and no IDs it deletes nothing, prints the command naming each entry and exits 2 (3 while an unreadable entry remains)
- **Needs:** 1 Machine · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon: 0.4 only
- **Docs:** services/volumes.md#get-deleted-data-back

### VOL-20 When a Volume's Server goes down, its Services wait and the mirror can take over
- **Surface:** dashboard
- **Reach:** a Volume on web-1 with a mirror on web-2; `power off` web-1; open the Volume's **Copies**; forget web-1 (**Forget server**, or `ployz server forget web-1 --confirm dead`); **Use mirror**; Deploy and confirm **Data loss**
- **Proof:** nothing moves on its own; Copies says `web-1 is not answering`, `Changes after <time> lost`, and **Live** reads **Not answering**; **Use mirror** shows only after the forget and only while the mirror is the last copy; after deploy the Service runs on web-2 and `md5sum` matches the mirror's last refresh; a CLI or Cloud move is refused while web-1 is down
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2 with verify-server `power off`; verify-volume-runs for Machines away
- **Compat:** none
- **Docs:** services/volumes.md#when-its-server-goes-down

### VOL-21 Copies shows where a Volume's data lives
- **Surface:** dashboard
- **Reach:** Volume → **Copies** with a writer, a mirror and an old copy; restart `ployzd` on the mirror's Server; run a Move
- **Proof:** **Live** names the writer as **Live**, **Read-only** during a Move's pause, **Waiting for its report** right after a restart, **Needs a newer Ployz** on an older Server; **Elsewhere** lists the mirror with its age (amber with **⚠** past an hour, `Not answering` beside it when its Server is down) and the old copy; Copies offers no forget while any report is pending
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=3
- **Compat:** Cloud→Server: a 0.3.19 Server's copy reads **Needs a newer Ployz**
- **Docs:** services/volumes.md#copies

### VOL-22 A deploy won't start a Service whose Volume has no writer
- **Surface:** CLI
- **Reach:** `ployz server rm` the writer's Server (its copy is demoted), then `ployz deploy`; repeat with the only copy on a removed Server; repeat with copies on two Servers
- **Proof:** the Service and its dependants read **Not attempted**, the deployment reads `Deployed, 2 waiting`, `ployz deploy` exits 3 and prints `Volume data has no writer; web-2 holds its only copy. Make it the writer: ployz volume restore data --from web-2`; with no copy it says no Server holds one; with an offline Server it names that Server and suggests no restore
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** none
- **Docs:** services/volumes.md#when-a-volume-has-no-writer

### VOL-23 Restore a Volume's writer without Ployz Cloud
- **Surface:** CLI
- **Reach:** `ployz volume restore data --from web-2` with `--context`/`--connect`, run twice; repeat with a third Server down or holding a copy
- **Proof:** prints `Restored data on web-2 from <UTC time>; writes after that time are lost.` and `next: ployz deploy`; the second run prints `data's writer is already on web-2; nothing to restore.` and exits 0; it refuses while any Server doesn't answer, while another Server holds a copy, or while a copy-holding Server runs a too-old Ployz
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server; verify-volume-runs
- **Compat:** CLI→daemon: 0.3.19's `volume restore` against 0.4 daemons
- **Docs:** services/restore-a-volume.md#restore-a-volume-in-place

### VOL-24 Restore a Volume's writer through Ployz Cloud
- **Surface:** CLI
- **Reach:** signed in, `ployz volume restore data --from web-2 --wait`. Also: MCP `volume_restore`; **Use mirror** in Copies (VOL-20).
- **Proof:** it prints a run id; `ployz volume runs data` ends done with `data restored on web-2 from <time>; writes after that time are lost`; it refuses naming the other copies when web-2 isn't the only one; in a Protected Environment a member's restore waits for an owner or admin
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`); verify-volume-runs
- **Compat:** Cloud→Server: check a 0.3.19 Server holding the copy is refused by capability
- **Docs:** services/restore-a-volume.md#restore-a-volume-in-place

### VOL-25 Restoring a point into a new Volume is turned off
- **Surface:** dashboard
- **Reach:** Volume → **Copies**, a mirror's or old copy's point
- **Proof:** in place of **Restore** it reads `Restoring a point into a new volume is turned off in this version of Ployz.`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** none
- **Docs:** services/restore-a-volume.md#restoring-a-point-is-turned-off-in-this-version

### VOL-26 Move a Volume and its Service to another Server
- **Surface:** CLI
- **Reach:** `ployz volume move data --to web-2 --wait` under the ack probe. Also: MCP `volume_move`. The dashboard has no Move button.
- **Proof:** it prints the run id; `ployz volume runs data` ends done; `ployz exec web -- md5sum /data/f` matches before and after and the container runs on web-2; the probe's longest gap covers only the final copy; Copies lists `data-web-1` under **Elsewhere** as an old copy that never refreshes; `ployz volume move data --to web-3` then stops with `error: data already has a copy on web-1, data-web-1` and `next: ployz volume mirror rm data-web-1 --if-old-copy`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`, `probe`); verify-volume-runs; verify-volume-marks for seed, promote and cut
- **Compat:** Cloud→Server: Cloud refuses a Move onto an older Server by capability (`Upgrade machine-1 to move pgdata`, `Volume move needs a newer Ployz on web-2`)
- **Docs:** services/move-a-volume.md

### VOL-27 A Move is refused before anything stops when the destination can't start the Service
- **Surface:** CLI
- **Reach:** `ployz volume move data --to web-2` where web-2 has no approved version, the registry login is wrong, the Service also mounts `uploads`, `worker` also uses `data`, or web-2 runs an older Ployz
- **Proof:** each refusal names its reason, like `web-2 can't start web: no approved version of web runs there. Deploy web so it can, then move data`, `web also mounts uploads, which can't move with data`, `web and worker all use data on web-1`; the Service keeps running on web-1 (container ID unchanged)
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`)
- **Compat:** Cloud→Server: the older-Ployz refusal is the 0.3.19 cell
- **Docs:** troubleshooting/volumes.md#a-move-says-the-other-server-cant-start-the-service

### VOL-28 A failed or stuck run is released
- **Surface:** CLI
- **Reach:** kill the Cloud runner or power off web-2 at each stage of a Move (before stop, after stop, after handover); then `ployz volume release data --wait`. Also: MCP `volume_release`.
- **Proof:** before stop the Service keeps running on web-1; after stop and before handover web-2's copy is deleted and the Service restarts on web-1; after handover `runs` shows the release that finishes on web-2; a silent Server gets `waiting for web-1 to answer (check 1 of 8)` for up to 15 minutes; a lost run cleans up on its own within about an hour; a move that never snapshotted prints `data on web-1 stopped before its first snapshot; ployz volume move data --to web-2`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`runner kill`, `power off/on`); verify-volume-runs for late steps from a dead run and restarts
- **Compat:** Cloud→Server: a release keeps copies on a ≤0.4.0-beta.2 Server and names them
- **Docs:** troubleshooting/volumes.md#a-move-failed-partway

### VOL-29 A Volume takes one run at a time, and a deploy takes a Move over
- **Surface:** CLI
- **Reach:** start a mirror, then `ployz volume move data --to web-3`; start a deploy in the Environment, then a volume command; start a Move and deploy before handover; deploy after handover with a volume change staged
- **Proof:** `data is busy with a mirror; ployz volume runs data <id> follows it, and ployz volume release data stops it`; `data is deploying; the Deploy ends its run`; the overtaken run reads `a Deploy took this run over` and the Service deploys on web-1; after handover the deploy fails with `data is starting on web-2; it finishes there`
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-volume-runs
- **Compat:** none
- **Docs:** troubleshooting/volumes.md#a-volume-command-says-the-volume-is-busy

### VOL-30 List and read a Volume's runs
- **Surface:** CLI
- **Reach:** `ployz volume runs data`, `ployz volume runs data <run-id>` (`--json`)
- **Proof:** one row per move, mirror, sync, release, restore and resize with how it ended; the detail names each step and the failure text
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** CLI→Cloud: 0.3.19 has `volume runs`; check it reads main Cloud's runs
- **Docs:** services/move-a-volume.md#the-old-copy-a-move-leaves

### VOL-31 Add a mirror in the dashboard
- **Surface:** dashboard
- **Reach:** a deployed Managed Volume → **Copies** → **Elsewhere** → **Add copy**, pick web-2, **Deploy**
- **Proof:** before deploy the mirror shows green, counts in **Details** as "Seed", and the canvas card reads **New**; after deploy the card reads `data-web-2 · Mirror · <age>` and `zfs list` on web-2 shows the read-only copy; with any Server on an older Ployz Copies only shows and names it ("Upgrade db-1 to a newer Ployz")
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: a 0.3.19 Server blocks mirrors until upgraded
- **Docs:** services/mirror-a-volume.md#add-a-mirror

### VOL-32 Mirror a Volume from the CLI
- **Surface:** CLI
- **Reach:** `ployz volume mirror data --to web-2 --wait`. Also: MCP `volume_mirror`.
- **Proof:** it runs at once without a deploy and prints a run id; Copies lists `data-web-2` as a mirror; onto an older Server it is refused with `Upgrade web-2 first. uploads needs a newer Ployz on every Server that holds it.`; a mirror onto a copy that may hold unique writes stops and keeps it
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-volume-marks for the pull's marks
- **Compat:** Cloud→Server: as VOL-31
- **Docs:** services/mirror-a-volume.md#mirror-from-the-cli

### VOL-33 Refresh a mirror, by hand or every 15 minutes
- **Surface:** CLI
- **Reach:** `ployz volume sync data`; also **Refresh now** on the mirror; wait 15 minutes; deploy
- **Proof:** it answers at once, like `data-web-2 is pulling w-3-1760340000`; the mirror reads **Refreshing**, then its age resets and `md5sum` on a mounted copy of the snapshot matches; the 15-minute and per-deploy refreshes reset the age without a command; during a Move **Refresh now** says a move is running; a mirror past an hour reads **Refresh skipped** with the reason
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard SERVERS=2 for the panel
- **Compat:** Cloud→Server: the 15-minute refresh waits while any Server is too old
- **Docs:** services/mirror-a-volume.md#refresh-a-mirror

### VOL-34 Copy a mirror again in full
- **Surface:** CLI
- **Reach:** `ployz volume sync data --full --wait`; then on an old copy
- **Proof:** the run deletes and re-copies the mirror; on an old copy, or one that may hold unique writes, it stops with `error: data-web-1 may hold writes no other Server has, so sync --full keeps it` and a `next:` line, and nothing is deleted
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** Cloud→Server: a ≤0.4.0-beta.2 Server gets no full sync (`Upgrade web-1 first. …`)
- **Docs:** troubleshooting/volumes.md#a-full-sync-says-the-copy-may-hold-writes-no-other-server-has

### VOL-35 Remove a mirror in the dashboard
- **Surface:** dashboard
- **Reach:** click the mirror → **Remove mirror**, **Deploy**, type `shop/production`, **Deploy**
- **Proof:** it shows red (**Removing**) and "Delete" in Details until the deploy; the deploy asks because data goes; after it `zfs list` on web-2 lacks the copy and its data is in web-2's trash
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** none
- **Docs:** services/mirror-a-volume.md#remove-a-mirror

### VOL-36 Delete a mirror or old copy from the CLI
- **Surface:** CLI
- **Reach:** `ployz volume mirror rm data-web-2 --wait`; `ployz volume mirror rm data-web-1 --if-old-copy`; on the last copy, with and without `--confirm data`
- **Proof:** the copy leaves `zfs list` into the trash; `--if-old-copy` on a copy that became the mirror prints `data-web-1 is no longer an old copy, so nothing was deleted`; the last copy is refused without `--confirm data`; in a Protected Environment a member's call waits for an owner or admin
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** Cloud→Server: a pre-`--if-old-copy` Cloud refuses the flag and deletes nothing; a beta.2 self-host Cloud can't remove a Move's old copy
- **Docs:** services/mirror-a-volume.md#mirror-from-the-cli

### VOL-37 The agent removes every read-only copy at once
- **Surface:** MCP
- **Reach:** `ployz mcp` (or the Agent sidebar) `volume_mirror_remove` on a Volume with a mirror and an old copy
- **Proof:** both copies leave `zfs list` on their Servers in one call; unlike `ployz volume mirror rm`, it takes no copy name; with no writable copy left, the result says the Volume's data goes too
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`verify-cluster up --cloud`)
- **Compat:** none
- **Docs:** **undocumented** (the agent's all-copies behavior; docs describe only the CLI's one copy)

### VOL-38 Delete an old copy from the dashboard
- **Surface:** dashboard
- **Reach:** Copies → **Elsewhere**, × on the old copy's row → **Delete copy** → **Delete**; refresh the copy between opening and confirming
- **Proof:** the row reads **Deleting**, then the copy leaves `zfs list` into web-1's trash; when the copy changed it says `web-1's copy changed since you opened Delete, so nothing was deleted. Look at it again before you delete it`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-dashboard SERVERS=2
- **Compat:** Cloud→Server: a ≤0.4.0-beta.2 Server is told to upgrade first
- **Docs:** services/move-a-volume.md#the-old-copy-a-move-leaves

### VOL-39 Servers keep a copy that may hold writes no other Server has
- **Surface:** daemon
- **Reach:** make an old copy by a Move, then try to delete it by every path except `mirror rm`/a confirmed Deletes data: a deploy's cleanup, a release, a mirror pull onto it
- **Proof:** each stops with `data's copy on web-1 may hold writes no other Server has; only ployz volume mirror rm deletes it` and `zfs list` still shows it; a release ends done naming `kept data-web-1, …`; a delete whose copy changed since it was confirmed says `… changed since its delete was confirmed, so nothing was deleted`
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-volume-marks for the `ployz:*` marks behind the decision
- **Compat:** Cloud→Server: a ≤0.4.0-beta.2 Server would delete or replace it, so Cloud refuses with `Upgrade web-1 first. …, so nothing was changed`; a pre-keep-written self-host Cloud shows `web-1 is busy with data`
- **Docs:** troubleshooting/volumes.md#ployz-keeps-a-copy-that-may-hold-writes-no-other-server-has

### VOL-40 List the copies deleted Volumes left behind
- **Surface:** CLI
- **Reach:** delete a Volume that had an old copy; `ployz volume leftovers` (`--json`). Also: `/cloud/ada/~/servers/<serverId>` → **Copies of deleted volumes** → **Copy delete command**.
- **Proof:** a row per kept copy with its Server, slot, last snapshot, size and whether it may hold unique writes; the deploy's cleanup run names it `kept data-web-1, … (ployz volume leftovers rm shop-production_vol-3f2a --on web-1 deletes it)`; the Server page lists the same copy
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard SERVERS=2 for the Server page
- **Compat:** CLI→Cloud: 0.3.19's CLI has no `volume leftovers`
- **Docs:** troubleshooting/volumes.md#copies-left-behind-by-a-deleted-volume

### VOL-41 Delete a leftover copy in two steps
- **Surface:** CLI
- **Reach:** `ployz volume leftovers rm shop-production_vol-3f2a --on web-1`, then the printed line with `--confirm <copy>`; repeat after the copy changes; ask the agent to do it
- **Proof:** the first run deletes nothing and prints `error: … slot 7, last snapshot …, up to 10.0 GB; may hold writes no other Server has. Run ployz volume leftovers rm … --confirm 7-1234-written to delete it`; the confirmed run asks yes and the copy moves to the trash; a changed copy stops again; the agent's attempt is refused naming the line for a person
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server
- **Compat:** Cloud→Server: on a ≤0.4.0-beta.2 Server it says `Its Ployz would delete shop-production_vol-3f2a's copy`
- **Docs:** troubleshooting/volumes.md#copies-left-behind-by-a-deleted-volume

### VOL-42 A Server opens a Volume's data only for the Service approved for it
- **Surface:** daemon
- **Reach:** leave data under the name `data` on web-1 from an earlier Volume, then deploy a new `data` Volume there; mount a provisioned Volume as an external mount; stop Docker during a Move
- **Proof:** the deploy fails with `Volume 'data' holds another Volume's data under the same name, not the data this Service was approved for` and nothing is deleted; the external mount fails with `… an External mount opens only a Volume made outside Ployz`; with Docker down `Docker is not answering on this Machine, so Volume data keeps its copies as they are until it does`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server; verify-volume-marks
- **Compat:** none
- **Docs:** troubleshooting/volumes.md#a-deploy-says-the-volume-holds-another-volumes-data

### VOL-43 A new Volume stays on the Server its first deploy chose
- **Surface:** CLI
- **Reach:** deploy a new Volume, kill the deploy before it is made, then change labels so that Server can't run the Service and deploy again; run two deploys at once
- **Proof:** `Volume data was placed on web-1, which can't run this service. Let web-1 run it, or remove data and add it again. It holds no data yet.`; the race reads `Another deploy placed volume data on a different server first. Deploy again.`
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: no (Standalone)
- **Drive:** verify-server; verify-volume-runs (first placement)
- **Compat:** none
- **Docs:** troubleshooting/volumes.md#a-deploy-says-the-volume-was-placed-on-another-server

### VOL-44 A drain moves each Service with its Volume
- **Surface:** CLI
- **Reach:** signed in, `ployz server drain web-1` with `db` on a Volume there and a mirror on web-3; kill the runner mid-copy; restart web-3's daemon during start
- **Proof:** the **Drain** row reads `Draining · Moving data to web-3` (the mirror's Server first); `ployz volume runs data` lists the move; `md5sum` matches on web-3; a Service with two Volumes stays with its reason; a failed move names its release run; draining web-3 meanwhile is refused with `data is moving to this server. Wait for run … to finish.`
- **Needs:** 3 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server (`runner kill`); verify-drain-copy for the new copy's start, adopt and resend
- **Compat:** Cloud→Server: an older drained Server reads `Upgrade web-1 to move data with the drain`
- **Docs:** servers/manage-servers.md#services-with-a-volume

### VOL-45 List and prune the anonymous volumes a Server kept
- **Surface:** CLI
- **Reach:** scale a Service whose image declares a `VOLUME` from 2 to 1 (or drain it); `ployz server volumes web-1`; `ployz server volumes web-1 prune --older-than 30d`; `ployz server volumes web-1 prune <name> --yes`; `prune </dev/null`
- **Proof:** the list shows each volume's Service, mount path, kept time and size, and `ployz server show web-1` counts them; prune shows what goes and asks; `--older-than 30d` keeps recent ones; it never deletes a volume a container uses; with no terminal it fails `confirmation_required`; a replace deploy deletes the old container's anonymous volume instead of keeping it
- **Needs:** 1 Machine · Volumes: Docker · Cloud: no (Standalone)
- **Drive:** verify-server
- **Compat:** CLI→daemon, runner→Server: a 0.4.0-beta.2 (or 0.3.19) deploy client makes a newer Server keep them on every replace; 0.3.19's CLI has no `server volumes`
- **Docs:** services/services.md#delete-the-volumes-a-server-kept

### VOL-46 Remove a Namespace no Environment owns
- **Surface:** CLI
- **Reach:** leave a Namespace behind (kill an `env rm` partway); signed in, `ployz server clean`, then `ployz server clean --namespace my-app-staging --confirm my-app-staging`; again with `--context`. Also: `/cloud/ada/~/servers/<serverId>` → **Running here** → **Namespaces no Project owns** → **Remove**; MCP `server_clean`.
- **Proof:** the list marks it **Not in any Project**; the removal asks yes or no, then `docker ps` and `zfs list` on every Server lack its containers and Volumes; without a terminal it fails `confirmation_required` naming the Volumes whose data goes; a member gets the owner-or-admin refusal and exit 1
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard SERVERS=2 for the Server page
- **Compat:** CLI→Cloud: 0.3.19's CLI has no `server clean`
- **Docs:** servers/manage-servers.md#clean-up-after-a-deleted-environment

### VOL-47 A Protected Environment holds a member's Volume runs for an owner
- **Surface:** CLI
- **Reach:** as a member (or a token without `environments:protect`), in a Protected Environment: `ployz volume move`, `restore`, `release`, `mirror rm`, `set --size`; then `ployz volume mirror` and `sync`
- **Proof:** each gated call is refused with **An owner or admin does this.** or waits for an owner or admin to approve (then `--approval <DIGEST>`); `mirror` and `sync` go through without asking. The docs disagree on which: move-a-volume.md says move, restore, release, mirror removal and resize are refused, while mirror-a-volume.md, restore-a-volume.md and volumes.md say `mirror rm`, restore and resize wait for approval. Record what each does.
- **Needs:** 2 Machines · Volumes: Managed (ZFS) · Cloud: local Cloud
- **Drive:** verify-server; verify-dashboard SERVERS=2 for the member view
- **Compat:** none
- **Docs:** services/move-a-volume.md#the-old-copy-a-move-leaves
