# Errors through Cloud (real Server)

## Errors human

### logs of an unknown Service 

```console
$ ployz logs nosuch
# exit 1
```

stderr:
```
error: No Service named nosuch in Environment production
valid: api, postgres, web, worker
```

### exec in an unknown Service 

```console
$ ployz exec nosuch -- true
# exit 1
```

stderr:
```
error: No Service named nosuch in Environment production
valid: api, postgres, web, worker
```

### exec a missing binary 

```console
$ ployz exec web -- no-such-binary
# exit 127
```

stdout:
```
OCI runtime exec failed: exec failed: unable to start container process: exec: "no-such-binary": executable file not found in $PATH
```

### exec in a stopped Service 

```console
$ ployz exec worker -- true
# exit 1
```

stderr:
```
error: Could not start the command in ployz-create-af57ae4c8ee193deb9af715b3ade4e060cb09821ea5c48dfe127555471401bf9.
  cause: Docker responded with status code 409: container c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d is not running
```

### logs of a stopped Service 

```console
$ ployz logs worker --tail 2
# exit 0
```

stderr:
```
2026-10-20T17:13:35.024512312+00:00 machine-1 worker/78810c66d7ae | exited with 2
2026-10-20T17:14:00.740444025+00:00 machine-1 worker/c72c2a83b060 | exited with 2
```

### service restart of an unknown Service 

```console
$ ployz service restart nosuch
# exit 1
```

stderr:
```
error: No Service named nosuch in Environment production
valid: api, postgres, web, worker
```

### service inspect of an unknown Service 

```console
$ ployz service inspect nosuch
# exit 1
```

stderr:
```
error: No Service named nosuch in Environment production
valid: api, postgres, web, worker
```

### deploy an unknown Service 

```console
$ ployz deploy nosuch
# exit 1
```

stderr:
```
error: No Service named nosuch to deploy
```

### deploy with a stale --expect-version 

```console
$ ployz deploy --expect-version 1:1:0.0
# exit 1
```

stderr:
```
error: The Environment changed after this review. Review the latest changes and try again
next: ployz deploy --expect-version 39:16:0.28
```

### deployment show of an unknown id 

```console
$ ployz deployment show 00000000-0000-0000-0000-000000000000
# exit 1
```

stderr:
```
error: No Deployment 00000000-0000-0000-0000-000000000000
```

### deployment show of an unknown number 

```console
$ ployz deployment show 9999
# exit 1
```

stderr:
```
error: shop/production has no Deployment #9999
next: ployz deployment ls
```

### deployment cancel of an applied Deployment 

```console
$ ployz deployment cancel 0d3dbff3-2ba6-4e79-a301-da532e3665d6
# exit 1
```

stderr:
```
error: This Deployment already ended
next: ployz deployment show 33
```

### deployment start of an applied Deployment 

```console
$ ployz deployment start 0d3dbff3-2ba6-4e79-a301-da532e3665d6
# exit 1
```

stderr:
```
error: This Deployment already ended
next: ployz deployment show 33
```

### domain check of an unknown domain 

```console
$ ployz domain check nosuch.example.com
# exit 1
```

stderr:
```
error: No such domain in Environment production
valid: api.ada.ployz.test, acme.com
```

### domain set on an unknown Service 

```console
$ ployz domain set nosuch foo
# exit 1
```

stderr:
```
error: No Service named nosuch in Environment production
valid: api, postgres, web, worker
```

### domain rm of an unknown domain 

```console
$ ployz domain rm nosuch.example.com
# exit 1
```

stderr:
```
error: No such domain in Environment production
valid: api.ada.ployz.test, acme.com
```

### server show of an unknown Server 

```console
$ ployz server show nosuch
# exit 1
```

stderr:
```
error: Could not show Server nosuch.
  cause: No Server named "nosuch"
inspect: ployz server ls
```

### env copy of an unknown node 

```console
$ ployz env copy nosuch --env fix-api
# exit 1
```

stderr:
```
error: This Branch uses no node named nosuch live; it uses only what its copies use
```

### env copy in a non-Branch 

```console
$ ployz env copy web
# exit 2
```

stderr:
```
error: production is not a Branch
next: ployz env branch NAME --from production --project shop
```

### env sync --to from a non-Branch 

```console
$ ployz env sync --to --plan
# exit 2
```

stderr:
```
error: production has no Parent: name where it syncs
next: ployz env sync --to ENV --project shop --env production
```

### ps in an unknown Environment 

```console
$ ployz ps --env nosuch
# exit 1
```

stderr:
```
error: No Environment named nosuch in Project shop
next: ployz env ls --project shop
valid: fix-api, hotfix, production, search, staging
```

### logs --since garbage 

```console
$ ployz logs web --since yesterday
# exit 2
```

stderr:
```
error: invalid time "yesterday" for --since: expected a relative duration, RFC 3339 date, or Unix timestamp
```

## Errors --json

### logs of an unknown Service --json

```console
$ ployz --json logs nosuch
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api","postgres","web","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### exec in an unknown Service --json

```console
$ ployz --json exec nosuch -- true
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### exec a missing binary --json

```console
$ ployz --json exec web -- no-such-binary
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### exec in a stopped Service --json

```console
$ ployz --json exec worker -- true
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### logs of a stopped Service --json

```console
$ ployz --json logs worker --tail 2
# exit 0
```

stdout:
```
{"container":"c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d","container_id":"c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d","deployment":"0d3dbff3-2ba6-4e79-a301-da532e3665d6","hook":null,"level":"info","line":"2026/10/20 17:14:14 Starting up on port 80","machine":"machine-1","message":"2026/10/20 17:14:14 Starting up on port 80\n","namespace":"shop-production","replica":"ployz-create-af57ae4c8ee193deb9af715b3ade4e060cb09821ea5c48dfe127555471401bf9","service":"worker","stream":"stderr","timestamp":"2026-10-20T17:14:14.658771335+00:00","ts":"2026-10-20T17:14:14.658771335Z"}
{"container":"c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d","container_id":"c72c2a83b060bfe3b4b903510d2fc6c5a4db5b0e68e28c0a6d11e3ad06d3d29d","deployment":"0d3dbff3-2ba6-4e79-a301-da532e3665d6","exit":{"code":2,"oom_killed":false},"hook":null,"machine":"machine-1","namespace":"shop-production","replica":"ployz-create-af57ae4c8ee193deb9af715b3ade4e060cb09821ea5c48dfe127555471401bf9","service":"worker","timestamp":"2026-10-20T17:15:10.417908542+00:00","ts":"2026-10-20T17:15:10.417908542Z"}
```

### service restart of an unknown Service --json

```console
$ ployz --json service restart nosuch
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api","postgres","web","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### service inspect of an unknown Service --json

```console
$ ployz --json service inspect nosuch
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api","postgres","web","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### deploy an unknown Service --json

```console
$ ployz --json deploy nosuch
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"services":["api","postgres","web","worker"]},"message":"No Service named nosuch to deploy"}}
```

### deploy with a stale --expect-version --json

```console
$ ployz --json deploy --expect-version 1:1:0.0
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"conflict","details":{"diff":{"version":"39:16:0.28"},"next":"ployz deploy --expect-version 39:16:0.28"},"message":"The Environment changed after this review. Review the latest changes and try again"}}
```

### deployment show of an unknown id --json

```console
$ ployz --json deployment show 00000000-0000-0000-0000-000000000000
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{},"message":"No Deployment 00000000-0000-0000-0000-000000000000"}}
```

### deployment show of an unknown number --json

```console
$ ployz --json deployment show 9999
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz deployment ls"},"message":"shop/production has no Deployment #9999"}}
```

### deployment cancel of an applied Deployment --json

```console
$ ployz --json deployment cancel 0d3dbff3-2ba6-4e79-a301-da532e3665d6
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"conflict","details":{"deployment":"0d3dbff3-2ba6-4e79-a301-da532e3665d6","next":"ployz deployment show 33"},"message":"This Deployment already ended"}}
```

### deployment start of an applied Deployment --json

```console
$ ployz --json deployment start 0d3dbff3-2ba6-4e79-a301-da532e3665d6
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"conflict","details":{"deployment":"0d3dbff3-2ba6-4e79-a301-da532e3665d6","next":"ployz deployment show 33"},"message":"This Deployment already ended"}}
```

### domain check of an unknown domain --json

```console
$ ployz --json domain check nosuch.example.com
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api.ada.ployz.test","acme.com"]},"message":"No such domain in Environment production"}}
```

### domain set on an unknown Service --json

```console
$ ployz --json domain set nosuch foo
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api","postgres","web","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### domain rm of an unknown domain --json

```console
$ ployz --json domain rm nosuch.example.com
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":["api.ada.ployz.test","acme.com"]},"message":"No such domain in Environment production"}}
```

### server show of an unknown Server --json

```console
$ ployz --json server show nosuch
# exit 1
```

stdout:
```
{"error":{"cause":["No Server named \"nosuch\""],"code":"not_found","details":{"inspect":["ployz server ls"]},"message":"Could not show Server nosuch."}}
```

### env copy of an unknown node --json

```console
$ ployz --json env copy nosuch --env fix-api
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"valid_children":[]},"message":"This Branch uses no node named nosuch live; it uses only what its copies use"}}
```

### env copy in a non-Branch --json

```console
$ ployz --json env copy web
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":{"next":"ployz env branch NAME --from production --project shop"},"message":"production is not a Branch"}}
```

### env sync --to from a non-Branch --json

```console
$ ployz --json env sync --to --plan
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":{"next":"ployz env sync --to ENV --project shop --env production"},"message":"production has no Parent: name where it syncs"}}
```

### ps in an unknown Environment --json

```console
$ ployz --json ps --env nosuch
# exit 1
```

stdout:
```
{"error":{"cause":[],"code":"not_found","details":{"next":"ployz env ls --project shop","valid_children":["fix-api","hotfix","production","search","staging"]},"message":"No Environment named nosuch in Project shop"}}
```

### logs --since garbage --json

```console
$ ployz --json logs web --since yesterday
# exit 2
```

stdout:
```
{"error":{"cause":[],"code":"invalid_argument","details":null,"message":"invalid time \"yesterday\" for --since: expected a relative duration, RFC 3339 date, or Unix timestamp"}}
```

