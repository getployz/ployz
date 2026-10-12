# Errors through Cloud (real Server)

## Errors human

### logs of an unknown Service 

```console
$ ployz logs nosuch
# exit 1
```

stderr:
```
No running Service "nosuch"; ployz ps lists what's running
```

### exec in an unknown Service 

```console
$ ployz exec nosuch -- true
# exit 1
```

stderr:
```
No running Service "nosuch"; ployz ps lists what's running
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
Machine RPC failed: Docker operation failed: Docker responded with status code 409: container c1f3947b56ef4cac9d549a166461acedf612dbe57569bfe34b61d82ba792cba7 is not running
```

### logs of a stopped Service 

```console
$ ployz logs worker --tail 2
# exit 0
```

stderr:
```
2026-10-05T02:32:23.672641169+00:00 machine-1 worker/c1f3947b56ef | 2026/10/05 02:32:23 Starting up on port 80
```

### service restart of an unknown Service 

```console
$ ployz service restart nosuch
# exit 1
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
No running Service "nosuch"; ployz ps lists what's running
```

### service inspect of an unknown Service 

```console
$ ployz service inspect nosuch
# exit 1
```

stderr:
```
No Service named nosuch in Environment production
valid: web, api, postgres, worker
```

### deploy an unknown Service 

```console
$ ployz deploy nosuch
# exit 1
```

stderr:
```
No Service named nosuch to deploy
```

### deploy with a stale --expect-version 

```console
$ ployz deploy --expect-version 1:1:0.0
# exit 1
```

stderr:
```
The Environment changed after this review. Review the latest changes and try again
next: ployz deploy --expect-version 68:37:0.64
```

### deployment show of an unknown id 

```console
$ ployz deployment show 00000000-0000-0000-0000-000000000000
# exit 1
```

stderr:
```
No Deployment 00000000-0000-0000-0000-000000000000
```

### deployment show of an unknown number 

```console
$ ployz deployment show 9999
# exit 1
```

stderr:
```
shop/production has no Deployment #9999
next: ployz deployment ls
```

### deployment retry of an applied Deployment 

```console
$ ployz deployment retry 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
# exit 1
```

stderr:
```
This Deployment applied, so there is nothing to retry
next: ployz deployment show 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
```

### deployment cancel of an applied Deployment 

```console
$ ployz deployment cancel 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
# exit 1
```

stderr:
```
This Deployment already ended
next: ployz deployment show 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
```

### deployment start of an applied Deployment 

```console
$ ployz deployment start 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
# exit 1
```

stderr:
```
This Deployment already ended
next: ployz deployment show 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
```

### domain check of an unknown domain 

```console
$ ployz domain check nosuch.example.com
# exit 1
```

stderr:
```
No such domain in Environment production
valid: acme.com, web.ada.ployz.test
```

### domain set on an unknown Service 

```console
$ ployz domain set nosuch foo
# exit 1
```

stderr:
```
No Service named nosuch in Environment production
valid: web, api, postgres, worker
```

### domain rm of an unknown domain 

```console
$ ployz domain rm nosuch.example.com
# exit 1
```

stderr:
```
No such domain in Environment production
valid: acme.com, web.ada.ployz.test
```

### server inspect of an unknown Server 

```console
$ ployz server inspect nosuch
# exit 1
```

stderr:
```
No Server named "nosuch"; ployz server ls lists them
```

### env copy of an unknown node 

```console
$ ployz env copy nosuch --env fix-api
# exit 1
```

stderr:
```
This Branch uses no node named nosuch live
```

### env copy in a non-Branch 

```console
$ ployz env copy web
# exit 1
```

stderr:
```
production is not a Branch
next: ployz env branch NAME --from production --project shop
```

### env sync --to from a non-Branch 

```console
$ ployz env sync --to --plan
# exit 1
```

stderr:
```
production has no Parent: name where it syncs
next: ployz env sync --to ENV --project shop --env production
```

### ps in an unknown Environment 

```console
$ ployz ps --env nosuch
# exit 1
```

stderr:
```
No Environment named nosuch in Project shop
next: ployz env new nosuch --project shop
```

### logs --since garbage 

```console
$ ployz logs web --since yesterday
# exit 1
```

stderr:
```
invalid log time "yesterday": expected a relative duration, RFC 3339 date, or Unix timestamp
```

## Errors --json

### logs of an unknown Service --json

```console
$ ployz --json logs nosuch
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":null,"message":"No running Service \"nosuch\"; ployz ps lists what's running"}}
```

### exec in an unknown Service --json

```console
$ ployz --json exec nosuch -- true
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### exec a missing binary --json

```console
$ ployz --json exec web -- no-such-binary
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### exec in a stopped Service --json

```console
$ ployz --json exec worker -- true
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"ployz exec does not support --json"}}
```

### logs of a stopped Service --json

```console
$ ployz --json logs worker --tail 2
# exit 0
```

stdout:
```
{"container_id":"c1f3947b56ef4cac9d549a166461acedf612dbe57569bfe34b61d82ba792cba7","hook":null,"machine":"machine-1","message":"2026/10/05 02:32:23 Starting up on port 80\n","service":"worker","service_id":"ec02437ef76a4d11a5af850934be313e","stream":"stderr","timestamp":"2026-10-05T02:32:23.672641169+00:00"}
{"container_id":"c1f3947b56ef4cac9d549a166461acedf612dbe57569bfe34b61d82ba792cba7","hook":null,"machine":"machine-1","message":"2026/10/05 02:32:52 Starting up on port 80\n","service":"worker","service_id":"ec02437ef76a4d11a5af850934be313e","stream":"stderr","timestamp":"2026-10-05T02:32:52.388788061+00:00"}
```

### service restart of an unknown Service --json

```console
$ ployz --json service restart nosuch
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":null,"message":"No running Service \"nosuch\"; ployz ps lists what's running"}}
```

stderr:
```
WARNING: Live Observation is observer-relative and not globally complete
```

### service inspect of an unknown Service --json

```console
$ ployz --json service inspect nosuch
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"did_you_mean":null,"valid_children":["web","api","postgres","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### deploy an unknown Service --json

```console
$ ployz --json deploy nosuch
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"services":["web","api","postgres","worker"]},"message":"No Service named nosuch to deploy"}}
```

### deploy with a stale --expect-version --json

```console
$ ployz --json deploy --expect-version 1:1:0.0
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":{"diff":{"changes":[],"environment":{"id":"4b4e5354-8834-4aea-a807-e65f832680bc","name":"production","project":"shop","revision":68},"follow_hints":[],"hints":[],"incoming":[],"published":true,"saved":37,"total_count":0,"version":"68:37:0.64"},"next":"ployz deploy --expect-version 68:37:0.64"},"message":"The Environment changed after this review. Review the latest changes and try again"}}
```

### deployment show of an unknown id --json

```console
$ ployz --json deployment show 00000000-0000-0000-0000-000000000000
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{},"message":"No Deployment 00000000-0000-0000-0000-000000000000"}}
```

### deployment show of an unknown number --json

```console
$ ployz --json deployment show 9999
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"next":"ployz deployment ls"},"message":"shop/production has no Deployment #9999"}}
```

### deployment retry of an applied Deployment --json

```console
$ ployz --json deployment retry 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":{"deployment":"0d67ecde-4da2-47d4-9eb0-bc4129ffd21f","next":"ployz deployment show 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f"},"message":"This Deployment applied, so there is nothing to retry"}}
```

### deployment cancel of an applied Deployment --json

```console
$ ployz --json deployment cancel 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":{"deployment":"0d67ecde-4da2-47d4-9eb0-bc4129ffd21f","next":"ployz deployment show 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f"},"message":"This Deployment already ended"}}
```

### deployment start of an applied Deployment --json

```console
$ ployz --json deployment start 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f
# exit 1
```

stdout:
```
{"error":{"code":"conflict","details":{"deployment":"0d67ecde-4da2-47d4-9eb0-bc4129ffd21f","next":"ployz deployment show 0d67ecde-4da2-47d4-9eb0-bc4129ffd21f"},"message":"This Deployment already ended"}}
```

### domain check of an unknown domain --json

```console
$ ployz --json domain check nosuch.example.com
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"valid_children":["acme.com","web.ada.ployz.test"]},"message":"No such domain in Environment production"}}
```

### domain set on an unknown Service --json

```console
$ ployz --json domain set nosuch foo
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"did_you_mean":null,"valid_children":["web","api","postgres","worker"]},"message":"No Service named nosuch in Environment production"}}
```

### domain rm of an unknown domain --json

```console
$ ployz --json domain rm nosuch.example.com
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"valid_children":["acme.com","web.ada.ployz.test"]},"message":"No such domain in Environment production"}}
```

### server inspect of an unknown Server --json

```console
$ ployz --json server inspect nosuch
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"No Server named \"nosuch\"; ployz server ls lists them"}}
```

### env copy of an unknown node --json

```console
$ ployz --json env copy nosuch --env fix-api
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"did_you_mean":null,"valid_children":[]},"message":"This Branch uses no node named nosuch live"}}
```

### env copy in a non-Branch --json

```console
$ ployz --json env copy web
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":{"next":"ployz env branch NAME --from production --project shop"},"message":"production is not a Branch"}}
```

### env sync --to from a non-Branch --json

```console
$ ployz --json env sync --to --plan
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":{"next":"ployz env sync --to ENV --project shop --env production"},"message":"production has no Parent: name where it syncs"}}
```

### ps in an unknown Environment --json

```console
$ ployz --json ps --env nosuch
# exit 1
```

stdout:
```
{"error":{"code":"not_found","details":{"next":"ployz env new nosuch --project shop"},"message":"No Environment named nosuch in Project shop"}}
```

### logs --since garbage --json

```console
$ ployz --json logs web --since yesterday
# exit 1
```

stdout:
```
{"error":{"code":"invalid_argument","details":null,"message":"invalid log time \"yesterday\": expected a relative duration, RFC 3339 date, or Unix timestamp"}}
```

