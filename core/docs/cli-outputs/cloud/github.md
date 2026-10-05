# Ployz Cloud CLI outputs: github

Captured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.

## github (GitHub App is fake in verify)

### github (bare)

```console
$ ployz github
# exit 2
```

stderr:
```
Connect GitHub so Services can build your repositories

Usage: ployz github [OPTIONS] [COMMAND]

Commands:
  connect     Print the GitHub App install link and wait until it is installed
  ls          List your GitHub installations and repositories, or one repository's branches
  disconnect  Forget one of your GitHub installations; uninstall the App on GitHub
  help        Print this message or the help of the given subcommand(s)

Options:
      --connect <connect>            [env: PLOYZ_CONNECT=]
      --ssh-timeout <SECONDS>        SSH setup timeout in seconds (provisioning: network connection only) [default: 20]
      --ployz-config <ployz-config>  [env: PLOYZ_CONFIG=] [default: ~/.config/ployz/config.yaml]
      --json                         Print the result as one JSON object on stdout
  -h, --help                         Print help (see more with '--help')
```

### github ls

```console
$ ployz github ls
# exit 0
```

stdout:
```
INSTALLATION	ACCOUNT	REPOSITORIES
No repositories yet. Next: ployz github connect
```

### github ls --json

```console
$ ployz github ls --json
# exit 0
```

stdout:
```
{
  "install_url": "https://github.com/apps/ployz-verify/installations/new",
  "installations": [],
  "linked": false,
  "next": "ployz github connect",
  "ready": false,
  "repositories": []
}
```

### github connect --json (no --wait)

```console
$ ployz github connect --json
# exit 1
```

stdout:
```
{"error":{"code":"unsupported","details":{"next":"ployz github connect"},"message":"GitHub reports installs by GitHub account, and yours isn't linked: sign in to Ployz Cloud with GitHub once, then rerun"}}
```

### github connect (waits; timed out by the capture)

```console
$ ployz github connect
# exit 1
```

stderr:
```
GitHub reports installs by GitHub account, and yours isn't linked: sign in to Ployz Cloud with GitHub once, then rerun
next: ployz github connect
```

### github disconnect unknown

```console
$ ployz github disconnect 12345
# exit 1
```

stderr:
```
No such GitHub installation of yours
next: ployz github ls
```

### github disconnect unknown --json

```console
$ ployz github disconnect 12345 --json
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"next":"ployz github ls"},"message":"No such GitHub installation of yours"}}
```

### github ls REPO (unknown repository)

```console
$ ployz github ls acme/shop
# exit 1
```

stderr:
```
No repository by that name that this Organization can read
next: ployz github ls
```

