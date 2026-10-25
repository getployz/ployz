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
- Values you save here are plain text. Seal secrets afterwards from their menu.

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
- To seal a plain variable, choose **Seal** from its menu. You can't unseal it. Saving the
  sealed value removes its earlier values from History too, so older versions show **Sealed**.
  A retried deployment of an older version gets the sealed value, not the one it had.
- To change a sealed value, delete the variable and add it again. Setting a sealed variable to
  a plain value is refused, so a secret never turns into plain text by mistake.
- Undo or Roll back in History never brings back an old sealed value. A sealed variable that
  History brings back comes back **Not set**: choose **Set value** to give it one. Until then,
  deploys leave it out of your service. A sealed variable added after the version you roll back
  to stays, with its value, on a service that stays. A service added after that version goes,
  with its variables. Delete a sealed variable yourself if you don't want it.
- A sealed value is stored exactly as typed, so it can't hold a
  [reference](#reference-another-services-variable).
- The [Ployz agent](../account/agent-sidebar.md) and `ployz mcp` can have Ployz make and seal
  a new random value, 32 letters, that they never see. They refuse a value you give them and name the
  `ployz set ... --secret` command to run instead.
- Asking for a new random value for a variable that's already sealed keeps the sealed value, so
  a retried change never swaps the password a database already uses. To get a new value, delete
  the variable first. A plain variable gets a new sealed value.
- Rotate a secret you saved before this change. Older backups of Ployz Cloud's database still
  hold it. Change the secret where it comes from, then delete the variable and add it again
  with the new value. A version saved before this change deploys without its sealed values, so
  deploy the current version instead.

## Reference another service's variable

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
  changing them leaves the parent alone. A branch never gets its parent's secret values, by
  design: each sealed value opens only in the environment it was sealed for. So the branch's
  sealed variables start **Not set**, and deploys leave each out until you choose **Set value**
  on it in the branch. A reference to a service the branch shares with its parent reads the
  parent's values.
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
