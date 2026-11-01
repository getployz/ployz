---
title: Variables
description: Configure your services with environment variables, secrets and references to other services.
---

Variables are the environment variables your service starts with: settings, API keys, your
database's URL. Each service has its own, and a variable can point at another service's, so
your app always gets the database of the environment it runs in.

## Add a variable

1. Open your service and go to the **Variables** tab.
2. Click **New Variable**, and enter a **Key** and a **Value**. Keys use letters, digits and
   underscores and can't start with a digit. Ployz uppercases what you type.
3. Tick **Sealed** for a secret: you can't read it back or unseal it. Leave it unticked for a
   value you want to see, or one that holds a [reference](#reference-another-services-variable).
   Click **Add**. If a value can't be sealed, the form says why and keeps what you typed.
4. Click **Deploy**.

<!-- screenshot: the Variables tab with a few variables, Raw editor and New Variable -->

To change or remove a variable, open the menu at the end of its row.

Your running service keeps its old values until you deploy. Changing a variable
[rebuilds](../builds/overview.md) a service built from GitHub on that deploy.

## Paste many at once

Click **Raw editor** to see the service's variables as a `.env` file (**ENV**) or as **JSON**.
Paste or edit them, then click **Update variables**.

- Deleting a line deletes that variable.
- Sealed variables stay as they are. To delete one, use its menu.
- Once you have a server, each new variable gets a **Seal** checkbox with its name under the
  editor. Tick it for a secret, and its value is sealed when you click **Update variables**.
  Unticked values are plain text.

## Rename a variable

From the CLI, `ployz set web.env.API_URL --rename PUBLIC_API_URL` renames a variable. Its value
moves with it, and a sealed variable stays sealed. Deploy to give your service the new name.

In Ployz Cloud, open the variable's menu in the service's Variables tab and choose Rename. If
Ployz refuses the new name, the dashboard shows why.

- Ployz refuses a new name another variable of the service already has, and leaves both as
  they are. Rename or remove that one first.

### Rename a variable other variables use

When other variables reference the one you rename, such as `${{ API_URL }}` in the same service
or `${{ web.API_URL }}` in another, they switch to the new name in the same change.

In Ployz Cloud, the Rename variable dialog lists them under **Also updates 2 references**.
Choose Rename to rename the variable and update them.

From the CLI, add `--update-references`:

```sh
ployz set web.env.API_URL --rename PUBLIC_API_URL --update-references
```

Without it, Ployz refuses to rename a referenced variable, lists the variables that use it, and
prints the command to run with `--update-references`. Nothing changes until you run it.

The [Ployz agent](../account/agent-sidebar.md) and `ployz mcp` can rename a referenced variable
too. Ask them to rename it, and they update its references in the same change. Like any rename,
it's staged until you deploy.

Only references in this environment's variables switch. Text that only looks like a reference,
such as an escaped `$${{ web.API_URL }}` or a sealed value, stays as it is. A Config file that
references the variable still blocks the rename: change that file first.

## Seal a secret

A value you seal is sealed on your device, in the browser or the `ployz` CLI, and opened only
on your servers. The dashboard never shows it again, and a lock marks it in the list. Your
service still gets the real value when it deploys.

- New variables are plain unless you tick **Sealed**.
- A new sealed value needs a server in your organization first. Add one with `ployz server add`.
  If your server shows **Needs recovery**, run `sudo ployz recover` on it instead
  ([recovery code](../servers/recovery-code.md#when-a-server-shows-needs-recovery)).
- Every server needs Ployz 0.3.4 or later to open sealed values, including a server you add
  after you seal one. Install a current `ployz` on the new server before you add it: an older one
  joins, but can't start your services there. See
  [A new server joined, but Global catch-up can't read another server](../troubleshooting/servers.md#a-new-server-joined-but-global-catch-up-cant-read-another-server).
- Your sealed values open only on your servers. Keep your organization's
  [recovery code](../servers/recovery-code.md): if you lose every server, it's the only way to
  open them again.
- To seal a plain variable, choose **Seal** from its menu. You can't unseal it. Saving the
  sealed value removes its earlier values from History too, so older versions show **Sealed**.
  A retried deployment of an older version gets the sealed value, not the one it had.
  An approval for a deploy that seals a variable doesn't keep its earlier value: its card shows
  **Sealed** on both sides of that change.
  What a branch made before the seal compares its changes with keeps only a fingerprint of the
  earlier plain value, so syncs and follows treat that value as unchanged without keeping it.
  The branch's record of how each service was first created still holds the earlier value
  until you delete the branch. Once the seal reaches the branch, its `ployz diff` and the
  agent's review show that earlier value as **Sealed**, even after the branch deletes the
  variable or adds it again plain. They also show it as **Sealed** when the branch deleted,
  renamed or changed the variable before the seal, including in a service it made separate
  after it was created, and in a branch made from a branch that still had the value it was
  created with. Discarding that change brings the variable back **Not set**, never with the
  earlier value. A service made separate before Ployz 0.3.12 still shows that earlier value
  there, and a discard brings it back. If the branch
  adds it again plain after taking the seal, its History keeps no copy of the earlier value
  either.
- To change a sealed value, delete the variable and add it again. Setting a sealed variable to
  a plain value is refused, so a secret never turns into plain text by mistake.
- Undo or Roll back in History never brings back an old sealed value. A sealed variable that
  History brings back comes back **Not set**: choose **Set value** to give it one. Until then,
  deploys leave it out of your service. A sealed variable added after the version you roll back
  to stays, with its value, on a service that stays. A service added after that version goes,
  with its variables. Delete a sealed variable yourself if you don't want it.
- Sealing a variable a sync replaced changes it, so undoing that sync is refused with
  `… is sealed since it synced: set it as you want it instead`. Undo never puts back the
  variable's earlier plain value: if you delete it, add back the plain value that arrived and
  save, Undo puts it back sealed and **Not set**. The record of what the sync brought keeps
  only a fingerprint of a value sealed since.
- A sealed value is stored exactly as typed, so it can't hold a
  [reference](#reference-another-services-variable) or [generate a value](#generate-a-value).
  The dashboard and `ployz set ... --secret` refuse a value containing `${{`. Leave the variable
  plain to use `${{ … }}`, or seal a value you supply.
- The [Ployz agent](../account/agent-sidebar.md) and `ployz mcp` can have Ployz make and seal
  a new random value, 32 letters, that they never see. They refuse a value you give them and name the
  `ployz set ... --secret` command to run instead. The refusal names the variable but never repeats
  the value, and offers no retry of the call. The same holds when they refuse such a call for any
  other reason, such as a field in the wrong shape.
- Asking for a new random value for a variable that's already sealed keeps the sealed value, so
  a retried change never swaps the password a database already uses. To get a new value, delete
  the variable first. A plain variable gets a new sealed value.
- Rotate a secret you saved before this change. Older backups of Ployz Cloud's database still
  hold it. Change the secret where it comes from, then delete the variable and add it again
  with the new value. A version saved before this change deploys without its sealed values, so
  deploy the current version instead.

## Generate a value

Write `${{ secret() }}` as a variable's whole value, and Ployz makes a random one for it, like a
database password. It's plain, not sealed, so you can read it and other variables can reference it.

```sh
POSTGRES_PASSWORD=${{ secret() }}
API_KEY=${{ secret(48) }}
PIN=${{ secret(8, "0-9") }}
```

- `secret()` makes 32 letters and digits. Give a length from 8 to 256, and optionally the
  characters to use, in quotes: ranges like `a-z` and single characters, at least two different
  ones. Spaces, quotes (`"` and `'`) and backslashes are refused, even inside a range such as
  `!-~`, so a value pastes into a shell or connection string as it is.
- Each environment makes its own value. A separate copy of a service in a branch or a
  [preview](../environments/preview-environments.md) gets a new one, so its new, empty database
  starts with a password of its own. A service the branch shares with its parent keeps the
  parent's value, and references to it read the parent's.
- A value stays until you change it. Deploys, History and Discard never make a new one, and
  neither does writing `${{ secret() }}` again, or setting the value it shows back.
- [Sync](../environments/environments.md) and pull request merges never carry a generated value.
  A new generated variable, or a new length or set of characters, arrives as `${{ secret(…) }}`,
  and the receiving environment makes its own value.
- To make a new value, write a different `secret(…)`, or set a plain value of your own. A
  database reads its password only when it first starts with an empty volume, so change it in
  the database too.
- `${{ secret() }}` must be the whole value. Write `$${{ secret() }}` for that text itself.
- Before Ployz 0.3.6, a value typed as `${{ secret(32) }}` was saved as that text. Values saved
  then stay as they are; typing it now makes a value.


A reference fills a variable with another variable's value. Write `${{ SERVICE.KEY }}` for
another service's variable and `${{ KEY }}` for one of this service's own:

```sh
DATABASE_URL=${{ postgres.DATABASE_URL }}
WEB_URL=http://${{ web.PLOYZ_PRIVATE_DOMAIN }}:${{ web.PORT }}
```

Leave **Sealed** unticked, then type `${{` in the value. The dashboard suggests this service's
variables, the [variables Ployz provides](#variables-ployz-provides), and other services'
exported variables. Tick **Exported** on a variable to add it to the suggestions; you can type a
reference to any variable either way.

![The New Variable dialog suggesting postgres.DATABASE_URL as you type a reference](../images/variable-reference.png)

- A reference reads the service of that name in the same environment. In staging,
  `${{ postgres.DATABASE_URL }}` is staging's database.
- A reference to a sealed variable keeps the secret hidden. Your service gets the real value.
- A service that references another starts after it on every deploy.
- A reference must name a service in the environment when you write it. A reference to a
  variable that doesn't exist, or to a service or variable you delete later, is a **Broken
  reference**: Ployz doesn't stop you, and shows it in red on the variable, in **Details**,
  `ployz diff` and `ployz status`. Deploys leave the variable out, and any variable that
  references it, and warn, until you change it.
- A reference to a deleted service shows its last name, as `${{ <deleted redis>.KEY }}`. Two
  deleted services of one name show as `<deleted redis>` and `<deleted redis#2>`, in the order
  you deleted them, and keep those names. A branch shows a service its parent deleted by the
  parent's name for it. A service deleted before Ployz recorded names shows by the end of its
  ID, as `<deleted #3f2a9c01>`. Edit the rest of the value or file
  around it and it stays; you can't write a new one.
- Write `$${{` for a literal `${{`.

## Variables Ployz provides

| Variable | Value |
| --- | --- |
| `PORT` | `8080`, unless you set it. Domains without a target port, and the healthcheck of a service without domains, reach your app on this port. |
| `PLOYZ_PRIVATE_DOMAIN` | The service's [private address](private-networking.md), like `web.internal` |
| `PLOYZ_SERVICE_NAME` | The service's private name, like `web` |
| `PLOYZ_ENVIRONMENT_NAME` | The environment's name, like `production` |
| `PLOYZ_SERVICE_ID`, `PLOYZ_ENVIRONMENT_ID` | The service's and the environment's IDs |

Of these, only `PORT` reaches your app on its own. To read another one, reference it in a
variable of your own, like `SELF_HOST=${{ PLOYZ_PRIVATE_DOMAIN }}`. The **Variables** tab lists
them, with their values, under your own variables.

Ployz also sets `PLOYZ_MACHINE_ID` in every container, to the ID of the server it runs on. You
can't reference it, and it replaces a variable of yours with that name.

## Good to know

- **Builds see your variables, sealed ones included.** A Dockerfile can keep what it reads in
  the image, so don't pass long-lived secrets to it as `ARG`s. See
  [Use variables at build time](../builds/railpack-and-dockerfiles.md#use-variables-at-build-time).
- **Branches copy variables, not secrets.** A separate copy of a service in a
  [branch](../environments/environments.md) or a
  [preview](../environments/preview-environments.md) starts with its parent's variables, and
  changing them leaves the parent alone. By design, a branch never gets its parent's secret
  values: its sealed variables start **Not set**, and deploys leave each out until you choose
  **Set value** on it in the branch. That includes a value the parent sealed but hasn't deployed
  sealed yet: a separate copy made then starts with it **Not set**, never with the earlier plain
  value the parent still runs. A [generated value](#generate-a-value) gets a new value in
  the copy. A reference to a service the branch shares with its parent reads the
  parent's values, except sealed ones: the branch doesn't get those either, so deploys leave
  the referencing variable out and say what it reads, such as
  `Left DB_URL out of web: it reads db.PASSWORD, which depends on a sealed value this branch doesn't get from its parent.`
  To give it a value, set the referencing variable in the branch. Or make the service separate
  in the branch (**Make it separate**, or `ployz env copy db --env <branch>`) and set the
  variable it names in the copy. A separate copy's volumes start empty.
- **One CLI or agent change holds up to 4 MiB.** A larger `ployz set` or agent edit fails with
  `invalid_argument` and the limit. Split it into smaller changes.
- **The CLI reads at most 32 MiB of an env file or a `--secret` on stdin.** A wrong path such
  as `/dev/zero` fails at once with `invalid_argument`. Anything smaller is judged by the limits
  here.
- **A variable holds up to 128 KiB, name included.** Linux won't start a program with a longer
  one, so Ployz refuses such a value when you set it, and refuses a deploy where references
  make one longer. When a secret makes it longer, the Deployment fails before anything runs and
  names the variable. Put a large value in a [Config](configs.md) file instead.
- **A service's variables should total at most 2 MiB.** Linux passes a program its variables and
  arguments together in a quarter of its stack limit, counting a few bytes more per variable for
  its bookkeeping. With Docker's default 8 MiB stack, that's 2 MiB, so a `set` whose plain values take a
  service past it, including `set --from-env-file`, lands with a note naming the total. Past
  6 MiB no server can start it, so Ployz refuses such a `set`, and a deploy where references or
  secrets resolve past it. Move bulk settings into a [Config](configs.md) file.

Next: [add a database](databases.md).
