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
before Ployz 0.3.4 may show none. It has nothing to undo, so its Undo is disabled and says **Sealed. Nothing to undo.** History does not recover deleted Volume data or undo private-image credential rotations.
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

[The agent](../account/agent-sidebar.md) can do the same, and its redeploys wait for you to approve.

## Roll back on servers that enforce device approval

With [device enforcement](../servers/manage-servers.md#require-a-devices-signature-for-restarts-and-joins)
on, redeploy or roll back from an approved device:

```bash
ployz deployment redeploy 4
```

Your device approves each service GitHub built for #4 under your
[automatic deploy rule](github.md#deploy-on-push-to-servers-that-enforce-device-approval), and
prints one `Approved` line per service. Each runs the image GitHub built for #4's commit. Your
device checks GitHub's signature on that image itself, and that it was built from #4's commit
with your rule's settings for your cluster. Ployz Cloud can't swap it. If it doesn't match,
your device approves nothing and cancels the redeploy:

```text
Cloud's image token for web at 3f1c2a9 doesn't check out: it names another commit. Nothing deployed.
```

GitHub builds the commit again when no image of it can be checked any more, for example when
GitHub has since rotated the key that signed its record, or the rule's settings changed since.
Your device approves that build first. GitHub builds it on the rule's branch, and your servers
release the build's secrets only for a commit on that branch's history. The redeploy waits,
printing `Building web at 3f1c2a9 on GitHub`, then your device approves the new image. The
approval's 15 minutes start after the build, so a long build doesn't use them up. A build that
fails cancels the redeploy, and nothing deployed.

- **The rule's workflow runs the rebuild.** It needs the `workflow_dispatch` trigger with a
  `commit` input, shown in [the workflow](github.md#deploy-on-push-to-servers-that-enforce-device-approval).
  A rule approved before Ployz 0.4 doesn't cover it: approve the rule again, then redeploy.
- **Keep the CLI running.** The redeploy waits for your device's approval. If you stop the CLI
  while it waits, it cancels the redeploy. If the CLI can't cancel it, Ployz Cloud fails it
  after an hour, and nothing deploys.
- **A redeploy from the dashboard or by the agent isn't approved.** Servers that enforce refuse
  it. Run `ployz deployment redeploy` on an approved device.
- **A rollback keeps your rule's settings.** Your device approves each service only with the
  settings your rule approves now: its command, environment, ports, mounts, health check and
  pre-deploy command. A redeploy of a deployment saved with other settings is refused, and
  nothing deploys. To roll those back, change the settings back and approve the rule again.
- **Only services GitHub built under your rule redeploy here.** A redeploy that ships a
  service run from an image, built from an upload or built on your servers is refused, and
  nothing deploys. Deploy those with `ployz deploy`.
