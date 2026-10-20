# Ployz Cloud CLI outputs: github

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## github (GitHub App is fake in verify)

### github (bare)

```console
$ ployz github
# exit 0
```

stdout:
```
Connect GitHub so Services can build your repositories

Usage: ployz github [OPTIONS] [COMMAND]

Commands:
  cat         Print one file of a repository
  connect     Print the link that connects a GitHub account and wait until it is connected
  disconnect  Disconnect a GitHub account from this Organization; uninstall the App on GitHub
  ls          List this Organization's GitHub accounts and repositories, or one repository's branches
  tree        List a repository's files
  help        Print this message or the help of the given subcommand(s)

Options:
      --connect <connect>      Reach the Servers through this connection instead of Cloud or a context [env: PLOYZ_CONNECT=]
      --ssh-timeout <SECONDS>  SSH setup timeout in seconds (provisioning: network connection only) [default: 20]
      --json                   Print the result as one JSON object on stdout; streams print one per line
      --color <WHEN>           When to color output; overrides NO_COLOR and CLICOLOR [default: auto] [possible values: auto, always, never]
  -h, --help                   Print help (see more with '--help')

```

### github ls

```console
$ ployz github ls
# exit 0
```

stdout:
```
REPOSITORY	VISIBILITY	DEFAULT BRANCH
```

stderr:
```
No repositories yet.
next: ployz github connect
```

### github ls --json

```console
$ ployz github ls --json
# exit 0
```

stdout:
```
{
  "connect_url": "http://localhost:35249/api/github/connect?organization=ada",
  "installations": [],
  "next": "ployz github connect",
  "ready": false,
  "repositories": []
}
```

### github connect --json (no --wait)

```console
$ ployz github connect --json
# exit 0
```

stdout:
```
{
  "connection": {
    "connect_url": "http://localhost:35249/api/github/connect?organization=ada",
    "installations": [],
    "ready": false,
    "repositories": []
  },
  "next": "ployz github connect --wait",
  "status": "pending",
  "url": "http://localhost:35249/api/github/connect?organization=ada"
}
```

### github connect (waits; timed out by the capture)

```console
$ ployz github connect
# exit 124
```

stderr:
```
Connect GitHub: http://localhost:35249/api/github/connect?organization=ada
Waiting for GitHub... 
```

### github disconnect unknown

```console
$ ployz github disconnect 12345
# exit 1
```

stderr:
```
error: No GitHub account by that login in this Organization
next: ployz github ls
```

### github disconnect unknown --json

```console
$ ployz github disconnect 12345 --json
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz github ls"},"message":"No GitHub account by that login in this Organization"}}
```

### github ls REPO (unknown repository)

```console
$ ployz github ls acme/shop
# exit 1
```

stderr:
```
error: No repository by that name that this Organization can read.
```

