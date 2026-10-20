# Ployz Cloud CLI outputs: org

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## org ls / use (second Organization babbage, owned by Ada)

### org ls

```console
$ ployz org ls
# exit 0
```

stdout:
```
ORGANIZATION	NAME	CURRENT
ada	Ada Lovelace's Projects	current
babbage	Babbage Labs	-
```

### org ls --json

```console
$ ployz org ls --json
# exit 0
```

stdout:
```
{
  "organizations": [
    {
      "current": true,
      "id": "5e293990-09ad-45c1-b636-6444de4fb848",
      "name": "Ada Lovelace's Projects",
      "slug": "ada"
    },
    {
      "current": false,
      "id": "47e4dfcf-7e63-44d0-9a6e-f183ca472a51",
      "name": "Babbage Labs",
      "slug": "babbage"
    }
  ]
}
```

### org (bare)

```console
$ ployz org
# exit 0
```

stdout:
```
List, make or switch Organizations

Usage: ployz org [OPTIONS] [COMMAND]

Commands:
  build-order  Show or set which Builders build Git Services, in turn
  leave        Leave the Organization you act in; this device moves to another of yours
  ls           List the Organizations you can act in
  new          Make an Organization you own; this device moves to it
  rm           Delete the Organization you act in, once it has no Project
  use          Act in another of your Organizations from this device
  approval     Commands: ls, show, approve, deny
  member       Commands: ls, invite, role, rm
  invite       Commands: ls, revoke
  help         Print this message or the help of the given subcommand(s)

Options:
      --connect <connect>      Reach the Servers through this connection instead of Cloud or a context [env: PLOYZ_CONNECT=]
      --ssh-timeout <SECONDS>  SSH setup timeout in seconds (provisioning: network connection only) [default: 20]
      --json                   Print the result as one JSON object on stdout; streams print one per line
      --color <WHEN>           When to color output; overrides NO_COLOR and CLICOLOR [default: auto] [possible values: auto, always, never]
  -h, --help                   Print help (see more with '--help')

```

### org use babbage

```console
$ ployz org use babbage
# exit 0
```

stdout:
```
This device now acts in Organization babbage.
```

### org use ada --json

```console
$ ployz org use ada --json
# exit 0
```

stdout:
```
{
  "organization": {
    "id": "5e293990-09ad-45c1-b636-6444de4fb848",
    "slug": "ada"
  }
}
```

### org use unknown

```console
$ ployz org use nope
# exit 1
```

stderr:
```
error: no Organization nope of yours
next: ployz org ls
```

### org use unknown --json

```console
$ ployz org use nope --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz org ls"},"message":"no Organization nope of yours"}}
```

## org build-order

### org build-order (show)

```console
$ ployz org build-order
# exit 0
```

stdout:
```
Auto: builds try GitHub, then your servers.
```

### org build-order --json

```console
$ ployz org build-order --json
# exit 0
```

stdout:
```
{
  "build_order": null,
  "builders": [
    "github",
    "servers"
  ]
}
```

### org build-order servers-only

```console
$ ployz org build-order servers-only
# exit 0
```

stdout:
```
Builds try your servers from the next build on.
```

### org build-order auto --json

```console
$ ployz org build-order auto --json
# exit 0
```

stdout:
```
{
  "build_order": null,
  "builders": [
    "github",
    "servers"
  ],
  "immediate": true
}
```

## org rm

### org rm ada without --confirm

```console
$ ployz org rm ada
# exit 2
```

stderr:
```
error: Removing Organization ada deletes it with its tokens, Servers' pairing and settings; this can't be undone. No changes made.
retry: ployz org rm ada --confirm ada
```

### org rm ada --confirm wrong

```console
$ ployz org rm ada --confirm wrong
# exit 2
```

stderr:
```
error: --confirm wrong does not match Organization ada. No changes made.
retry: ployz org rm ada --confirm ada
```

### org rm ada --confirm ada (has Projects)

```console
$ ployz org rm ada --confirm ada
# exit 1
```

stderr:
```
error: This Organization still has Projects (blog, shop). Remove them first
```

### org rm ada --confirm ada --json (has Projects)

```console
$ ployz org rm ada --confirm ada --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"conflict","details":{"next":{"input":{"project":"blog"},"tool":"remove"},"projects":["blog","shop"]},"message":"This Organization still has Projects (blog, shop). Remove them first"}}
```

### org rm babbage while acting in ada

```console
$ ployz org rm babbage --confirm babbage
# exit 2
```

stderr:
```
error: This credential acts in Organization ada, not babbage. No changes made.
next: ployz org use babbage
```

### org rm babbage --confirm babbage (empty, acting in it)

```console
$ ployz org rm babbage --confirm babbage
# exit 124
```

stderr:
```
This removal in Organization babbage destroys 1 thing:
  ✕ removes Organization babbage
Waiting for approval in Ployz Cloud… 
```

### org ls after removal

```console
$ ployz org ls
# exit 0
```

stdout:
```
ORGANIZATION	NAME	CURRENT
ada	Ada Lovelace's Projects	-
babbage	Babbage Labs	current
```

### status after removing the acting Organization

```console
$ ployz status
# exit 0
```

stdout:
```
cloud = http://localhost:35249
organization = babbage
signed in as = ada@example.com
needs attention = This Organization has no Project yet
```

stderr:
```
next: ployz project new NAME
```

