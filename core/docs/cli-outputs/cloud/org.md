# Ployz Cloud CLI outputs: org

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## org ls / use (second Organization babbage created through the auth API)

### org ls

```console
$ ployz org ls
# exit 0
```

stdout:
```
ORGANIZATION	NAME
ada *	Ada Lovelace's Projects
babbage	Babbage Labs
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
      "id": "a52ce0da-943d-4bfb-adfb-9da76e128533",
      "name": "Ada Lovelace's Projects",
      "slug": "ada"
    },
    {
      "current": false,
      "id": "d3d09d4f-a5b4-488c-8718-8a743f985dc8",
      "name": "Babbage Labs",
      "slug": "babbage"
    }
  ]
}
```

### org (bare)

```console
$ ployz org
# exit 2
```

stderr:
```
List or switch Organizations

Usage: ployz org [OPTIONS] [COMMAND]

Commands:
  ls           List the Organizations you can act in
  use          Act in another of your Organizations from this device
  build-order  Show or set which Builders build Git Services, in turn
  rm           Delete the Organization you act in, once it has no Project
  help         Print this message or the help of the given subcommand(s)

Options:
      --connect <connect>            [env: PLOYZ_CONNECT=]
      --ssh-timeout <SECONDS>        SSH setup timeout in seconds (provisioning: network connection only) [default: 20]
      --ployz-config <ployz-config>  [env: PLOYZ_CONFIG=] [default: ~/.config/ployz/config.yaml]
      --json                         Print the result as one JSON object on stdout
  -h, --help                         Print help (see more with '--help')
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
    "id": "a52ce0da-943d-4bfb-adfb-9da76e128533",
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
no Organization nope of yours
next: ployz org ls
```

### org use unknown --json

```console
$ ployz org use nope --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"next":"ployz org ls"},"message":"no Organization nope of yours"}}
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
# exit 1
```

stderr:
```
Removing Organization ada deletes it with its tokens, Servers' pairing and settings; this can't be undone. No changes made.
Retry: ployz org rm ada --confirm ada
```

### org rm ada --confirm wrong

```console
$ ployz org rm ada --confirm wrong
# exit 2
```

stderr:
```
--confirm wrong does not match Organization ada. No changes made.
```

### org rm ada --confirm ada (has Projects)

```console
$ ployz org rm ada --confirm ada
# exit 1
```

stderr:
```
This Organization still has Projects (shop). Remove them first
next: ployz project rm shop --confirm shop
```

### org rm ada --confirm ada --json (has Projects)

```console
$ ployz org rm ada --confirm ada --json
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":{"next":"ployz project rm shop --confirm shop","projects":["shop"]},"message":"This Organization still has Projects (shop). Remove them first"}}
```

### org rm babbage while acting in ada

```console
$ ployz org rm babbage --confirm babbage
# exit 1
```

stderr:
```
This credential acts in Organization ada, not babbage. No changes made.
next: ployz org use babbage
```

### org rm babbage --confirm babbage (empty, acting in it)

```console
$ ployz org rm babbage --confirm babbage
# exit 0
```

stdout:
```
Removed Organization babbage.
```

### org ls after removal

```console
$ ployz org ls
# exit 1
```

stderr:
```
this sign-in's Organization babbage is no longer one of yours
next: ployz org ls
```

### status after removing the acting Organization

```console
$ ployz status
# exit 1
```

stderr:
```
this sign-in's Organization babbage is no longer one of yours
next: ployz org ls
```

