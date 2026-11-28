---
title: History and rollback
description: See each saved version, undo a change, roll back to an earlier version, and redeploy or cancel a deployment.
---

Each time you save or deploy changes, Ployz keeps that version of the environment's settings. From that
history you can undo one version's changes, roll back to an earlier one, or ship an earlier
deployment again.

## Undo or roll back a saved version

Open **Deployments** in the sidebar, then **Versions**, to see this environment's saved versions.
Each version is titled from what it changed unless it was saved with a message, and lists its
changes one per row, folding past three. The first version lists what it created. **Live** marks the version the latest fully applied
deployment shipped. **Failed** marks the newest version when its
latest deployment failed; a later deployment of it clears it. A version saved but
not deployed yet has no badge. The version's **⋮** menu:

- **Undo these N changes** reverses that version's changed fields and keeps the versions saved
  after it. The first recorded version has no earlier version to undo.
- **Roll back to #N** stages that version's complete configuration, undoing every later version.
  The environment's [region](../services/regions.md) is the exception: it stays as it is.
  The newest version has nothing to roll back to. After a failed deploy, rolling back to the
  **Live** version deploys too: **Deploy** saves it, and services that already match are left
  running. Like every deploy, it moves GitHub services to the newest commit on their branch, so a
  branch that moved on since ships its newer code.

Each row's **⋮** undoes that one change. A change the same save tied to others undoes with them,
and the menu says how many: adding a Service together with another Service's reference to it,
a Volume together with its mounts, or a Source with its repository and branch. Every action stages
on the canvas; nothing is saved or deployed until you do.

If an action would overwrite draft changes, the confirmation lists exactly which fields.
Cancel changes nothing. If someone edits the environment after your review, reopen the action
to review the new draft. Save or Deploy makes the staged result a saved version.

Undo and Roll back pass the same checks as an edit made by hand. One that would reuse a name or
hostname another Service took since, or break a rule, is refused with the reason, and nothing is
staged. One that leaves a reference to a deleted Service stages, and shows it as a
**Broken reference**.

History restores configuration references. It never brings back a sealed value. A sealed
variable that still exists keeps its current value, and one added since stays. A sealed variable
deleted since, or one inside a Service that comes back, comes back **Sealed** and **Not set**. A
toast names those variables, the Service shows **· N not set**, and its Variables tab offers
**Set value** on each. Deploys leave a Not set variable out of the container until you set it.
A variable that was ever sealed can't come back as plain text. A version that only set sealed
values lists each variable it changed, shown **Sealed** or **Not set** without its value, though one saved
before Ployz 0.3.4 may show none. It has nothing to undo, so its Undo is disabled and says **Sealed. Nothing to undo.** History doesn't undo private-image credential rotations, and recovers deleted Volume data only after you [restore it from the server's trash](../services/volumes.md#get-deleted-data-back).
An accepted deployment and its redeploys keep their original snapshot while you edit the draft.

## Redeploy or cancel a deployment

- **Redeploy**, on any finished deployment, ships that deployment's commit and settings again.
  It removes services that aren't the environment's when the deployment it ships again did, and
  keeps and names them when that one was a **Deploy, keep other services**, a
  `ployz deploy --no-prune`, or made before deploys removed them.
  Your staged changes, volume changes included, wait for your next deploy, and **Copies wait
  for Deploy** beside the button says so when volume changes are staged. To ship a fix, change it
  and deploy instead. A deployment whose restore failed can't be redeployed, because it would mount
  the new volume empty. Redeploy refuses with "Restore failed. Restore again."
- **Roll back** by clicking **Redeploy** on a deployment older than one that ran, even one that
  failed partway. Ployz asks first, in **Roll back to #4**, and names what it undoes, such as
  **Undoes #5 and #6**. The rollback ships #4's commit and settings. Services added after #4 stop,
  and they and the other settings saved after #4 show as staged changes again. A volume #4 deleted
  that a later deployment brought back is deleted again only after you accept it. In **Deletes
  data**, type the project and environment, such as `shop/production`. The CLI refuses and prints
  the command that accepts it with `--accept-volume-loss`. If another deployment runs while you
  confirm, Ployz asks again with the new list.
  While a newer deployment is running, Redeploy on an older one refuses with "Wait for #5 to
  finish, then redeploy." A queued one gives way, as when you deploy again, and so does one whose
  runner stopped reporting.
- An applied removal or shutdown can't be redeployed: there is nothing left to ship. One that
  failed, was cancelled or reads `unknown` can.
- **A rollback lasts until your next push or deploy**, which ships your saved settings again. A
  deploy ships your latest commit too; a push, only to the services it builds. To keep the
  rollback, revert the commit in Git and change the settings back.
- **Cancel**, on a running deployment, stops it at its next step. What it already changed stays
  until your next deploy.
- On a queued deployment, **Deploy now** starts it if nothing is running ahead of it, and **⋮ →
  Cancel deployment…** takes it out of the queue. With no server in your organization, it refuses
  and says to add one, as a deploy does.

[The agent](../account/agent-sidebar.md) can't redeploy: it tells you to do it in Ployz Cloud.

## Roll back a service a rule built

A redeploy or rollback starts new containers. For a service that
[reaches your data](deployments.md#deploy-from-an-approved-device), a server takes it only when an
[approved device](../servers/manage-servers.md#what-needs-an-approved-device) signed that
version or an [automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push) covers it.

- **A service that runs from an image:** run `ployz deploy` on an approved device with the
  image you want back, as in
  [Deploy from an approved device](deployments.md#deploy-from-an-approved-device).
- **A service a rule built from GitHub:** use **Redeploy** or **Roll back** on the deployment
  that ran the build you want back. A server runs it under the rule while the rule's approval
  stands, even after the branch moved on. After you approve the rule again, it runs only builds
  pushed since, so revert the commit on the branch and push instead.
- **Redeploy** and **Roll back** in the dashboard, `ployz deployment redeploy` and a redeploy by
  [the agent](../account/agent-sidebar.md) sign nothing. They send the signature Ployz Cloud
  kept for each version that deployment ran, so a server runs a version a device signed, and
  any service that reaches no data. It refuses any other container, and the deployment fails
  without changing what runs.

## Forget older versions

To retire the approvals of older versions, deploy the version you want with
`ployz deploy --forget-older` on an approved device. It signs each version it ships marked to
forget, and every server then refuses a new container of an older version of those services
that opens a sealed value, mounts a volume, pulls its image with a login or reaches its server.
That includes the signatures Ployz Cloud kept, so **Redeploy** and **Roll back** to such an
older version fail too.

```bash
ployz deploy api --forget-older
```

A deploy of some services also ships each service they reference, so it forgets that
service's older versions too, and runs the version it signed for it.

- **Running containers keep running.** Forget only stops new ones. A stopped container that an
  older version made won't start again through Ployz when it needs that version's approval.
- **An older version that reaches no data can still run.** A server runs a service with no
  sealed value, no volume, no login for its image and nothing of its server without any
  signature, so a forget doesn't stop it. A rollback to one still works.
- **Every server is asked, not only the ones that run the service.** The command prints what
  each server did. A server that doesn't answer, refuses the forget, or runs a ployz too old to
  forget, is named, and the command exits 3. Run the same deploy again once it answers. A server that joins later takes
  what the server that admits it has forgotten, so a forget sent while it joins misses it until
  you send it again.
- **Automatic deploy rules stop.** A forget also retires each
  [automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push) for those services that
  was approved before it. Pushes stop deploying them, and Ployz Cloud can't move or drain the
  containers a rule built, until you click **Approve again** under **Automatic deploys**.
  Approving again approves the commit now at the head of the rule's branch, so check that
  commit first.
- **Forget goes by each device's clock.** The forget's time is your device's clock when you said
  yes. A version signed on a device whose clock runs behind yours can be forgotten even if it was
  signed after, and one signed on a device whose clock runs ahead can survive. Keep your devices'
  clocks set automatically. A server refuses a forget stamped more than 10 minutes past its own
  clock, and the command exits 3. If that happens, check the server's clock as well as yours.

`--forget-older` needs a deploy your device signs; see
[Deploy from an approved device](deployments.md#deploy-from-an-approved-device).
