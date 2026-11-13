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
on, redeploy or roll back from the dashboard with your passkey, as in
[Other deploys the dashboard starts](deployments.md#other-deploys-the-dashboard-starts), or from an
approved device:

```bash
ployz deployment redeploy 4
```

Your device shows everything it signs and asks you to approve the commit of each service #4
built, under your [automatic deploy rule](github.md#deploy-on-push-to-servers-that-enforce-device-approval):

```text
Approving commits for this redeploy:
  web at 3f1c2a9b8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b
  rule 5e0c2b7a91d84f36a0e9c1b2d3f4a5b6, generation 2
  cluster "q8Xc0mZ1vN4yT7rE2wK9pL3sD6fH5jG8aB1nM0oQ4uI="
  signer device k3m8q2xw7d
  valid 2026-11-11T03:19:09Z to 2026-11-11T03:34:09Z
Once each image is built or reused, this device approves the same commits again, without asking.
Approve these commits? [y/N]
```

`--yes` approves without asking. Outside a terminal, the redeploy needs `--yes`.

Your approval takes the place of the branch's head, and nothing else. Each server still runs only
the image GitHub built from that commit under your rule, with the settings your rule approves,
and checks all of it itself. Ployz Cloud can't swap the image, the settings or the commit.

A server that still holds the image GitHub built for #4 runs it again. Otherwise GitHub builds
the commit again on the rule's branch, and your servers release the build's secrets only for a
commit in that branch's history. The redeploy waits, printing `Building web at 3f1c2a9b8e7d on
GitHub`. Once every build ends, your device approves the same commits again with new times,
so a long build doesn't use up the approval's 15 minutes. A build that fails cancels the redeploy, and nothing
deploys.

- **The rule's workflow runs the rebuild.** It needs the `workflow_dispatch` trigger with a
  `commit` input, shown in [the workflow](github.md#deploy-on-push-to-servers-that-enforce-device-approval).
  A rule approved before Ployz 0.4 runs only on push: approve the rule again, then redeploy.
- **Keep the CLI running, or the tab open.** The redeploy waits for your device's second
  approval. If you stop the CLI or close the tab while it waits, nothing deploys. The CLI
  cancels the redeploy; otherwise Ployz Cloud fails it after an hour.
- **Each approval lasts 15 minutes.** A rebuild that starts later, such as one queued behind
  another deployment, fails, and the redeploy is cancelled. A server that reaches a service more
  than 15 minutes after your second approval refuses it, and the redeploy fails. Redeploy again.
- **A redeploy by the agent isn't approved.** Servers that enforce refuse it. Redeploy from the
  dashboard, or run `ployz deployment redeploy` on an approved device.
- **A deployment that built some services and ran others from an image** approves only the
  commits, so your servers refuse the services that run from an image. Deploy those with
  **Deploy** or `ployz deploy`.
- **A rollback keeps your rule's settings.** Your servers run each service only with the
  settings your rule approves now: its command, environment, ports, mounts, health check and
  pre-deploy command. A redeploy of a deployment saved with other settings is refused. To roll
  those back, change the settings back and approve the rule again.
- **It runs under your rule as it stands.** If you approved the rule again since #4, GitHub
  builds the commit again under the new approval. If you ended the rule, your servers refuse the
  rollback.
- **Each rebuild runs on GitHub, under your rule.** A service your servers built for #4 builds
  again on GitHub. One your rule doesn't deploy fails its build, and the redeploy is cancelled.
  A service built from an upload is refused before anything happens, and your servers refuse one
  that runs from an image. Deploy those with `ployz deploy`.
- **A pre-deploy command runs again.** Each approval runs a service's pre-deploy command once
  more, even for the commit that's running now.
- **GitHub retires the keys that sign its build records.** Your servers refuse an image whose
  record GitHub signed with a key it no longer publishes, and name the key. A redeploy can't
  build that commit again yet.
- **Every server needs a Ployz that checks your approval.** If one runs an older Ployz, the CLI
  says so and deploys nothing.
