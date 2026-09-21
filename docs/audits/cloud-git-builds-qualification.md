# Cloud Git build qualification

Live qualification on 2026-09-21. This report records measured results, including
the cancellation defect found during the run and its successful requalification.

## Deployed code and scope

- Reviewed code: `39fc17842b906bbebd32d1edbbf568decf2b4c6d`.
- Railway deployment: `4cc52c71-012d-4872-b9a4-b13f128cd3fe`; remote revision file
  matched that SHA and the native SDK loaded.
- Final cancellation correction and cleanup: reviewed code
  `74952ac7f5c1ced727ec2814684982775c693043`, Railway deployment
  `cb2af351-e21e-4f9c-bfef-ee14ddd8b0d6`. Remote revision, native SDK, public auth,
  signed workflow execution and private registration were verified again.
- Existing VPS: `ployz-dev`, `149.28.186.137`, machine
  `fd78a2aac76a44d197f586e14e23b822`, beta38 daemon, Docker 29.8.1, x86_64.
  No daemon protocol change or upgrade was required.
- Private source: `getployz/ployz-beta-fixture`, branch
  `spec930-build-qualification`, commit
  `b314ca6d19c19bf700ea6ded73dfa1d06466e30d`.
- Isolated fixture: project `payable-harrier`, environment
  `6ca50ed8-39e2-432a-9d16-2ef892599802`, namespace
  `payable-harrier-production`, service `db851e0d-51a1-4f79-b472-d496753af934`.

An external bundled qualification driver invoked existing authorized domain
commands with the existing organization member Actor. It created the fixture,
authored settings, submitted the normal reviewed publication, and let the real
Inngest deployment workflow execute. It did not insert deployment rows or forge
browser sessions. This qualifies the domain/workflow/runtime path; it does not
claim a browser authentication/UI smoke test.

## Results

All times below are UTC. Initial cases recorded source SHA `b314ca6d...`; the
final cancellation retest recorded `e2daad27b2c2d3849a3a06fec18f3a37698f0adc`
from branch `spec930-cancel-qualification`. That branch adds `Dockerfile.cancel`
with an uncached `sleep 961` step. Cleanup has no Git source.

| Case | Attempt | Start → finish | Result |
| --- | --- | --- | --- |
| Dockerfile | `1002c0fd-0142-4c68-959c-faad23b0e4e0` | 13:26:33.868 → 13:27:26.293 | Applied; selected `ployz-dev`, built and delivered image, running application returned expected health and identity responses. |
| Cached Dockerfile | `77b336b1-ffea-4f91-a8c2-2fb5efe01c8b` | 13:27:47.947 → 13:28:06.363 | Applied; actual BuildKit `#7 CACHED` and `#8 CACHED` output, 18.4 seconds. |
| Railpack | `b3ee02fc-31cc-4391-93c3-d18e124d209a` | 13:29:19.443 → 13:31:15.052 | Applied; public HTTPS `/health` and `/identity` returned 200 with expected fixture JSON. |
| Deliberate failure | `98543c59-0b58-4e82-a44b-f7ff71e63982` | 13:32:02.499 → 13:32:15.951 | Expected failure during Building; bounded output contains exit 42; no transfer/ready phase; previous applied container unchanged. |
| Explicit Retry | `f7170151-c064-44ff-b532-74d261b30956` | 13:32:46.315 → 13:32:59.740 | Expected repeated failure; recorded SHA and settings preserved. |
| Quiet cancellation | `a02a41c7-e413-410e-b5c1-70070077a295` | 13:33:32.201 → 13:34:23.085 | **Defect:** remote cancellation terminated the build and cleaned its builder, but Cloud recorded failed instead of cancelled. |
| Long quiet completion | `7678b896-4ee9-4dfb-bbb5-675a03aa58d6` | 13:36:12.852 → 13:52:36.071 | Applied; uncached 960-second build, 983.2-second live Inngest step, public HTTPS health 200. |
| Cancellation retest | `85a44701-0372-4023-9aab-d015cc0ec5ea` | 14:03:21.722 → 14:04:23.683 | **Passed on final code:** cancelled / `sdk_preparation_cancelled`; actual sleep 961 process and builder stopped; previous application stayed healthy. |
| Fixture cleanup | `f1073af6-177d-4ed3-92e9-ccbe88a50baa` | 14:05:25.298 → 14:05:41.014 | Applied through normal reviewed deletion; fixture container removed. |

The fixture initially authored port 8081 based on its image default. Runtime supplies
`PORT=8080`; the application was healthy there. The fixture route was corrected
through the ordinary authoring command to target 8080 before Railpack qualification.
The resulting public HTTPS route returned the expected identity payload. This was
a qualification configuration correction, not a source-code change.

Failure and Retry retained the Railpack container
`5adbeb9bb9b614d3860885d1098886458eeb261a425369d07c0bfab3cba08d9f`
and its immutable image digest
`sha256:80838aa6f89b792a3951f6b98d867c9022e206bedb40dba2d39322bc5994c2b5`.
Its public health response remained 200.

## Isolated connection loss

A separate Node child owned only its own SDK connection and an unconfirmed fixture
preparation. The parent used the normal authorized Git acquisition and canonical
pairing loader. For this fault test only, its temporary Dockerfile added a fresh
UUID stdout marker before `sleep960`; this prevented caching and distinguished
actual execution from a cached command description. Main Cloud cases used the
unchanged pinned source.

After observing the timestamped BuildKit marker, the parent killed that child with
SIGKILL. No deployment confirmation occurred. By 13:36:03 the observed sleep process
PID 17142 and its BuildKit container were gone. The daemon reported a missing
temporary Build tag during cleanup; no host quarantine conclusion is inferred.
The parent scope removed the temporary checkout.

This is real isolated SDK connection loss and server cleanup. It is narrower than
killing a complete live Cloud workflow: unknown-outcome mapping remains covered
at the deterministic Cloud workflow seam. No shared Cloud session, daemon, relay,
or user workload was stopped for the fault.

## Private networking and duration

All 23 registered function callbacks used the private origin
`http://web.railway.internal:8080` and path `/api/inngest`, including normal
function/step query parameters. Unsigned private POST returned 401; public auth
returned 200. The live deployments above executed through those registered callbacks.

The uncached quiet step started 13:36:26; host process PID 18262 was observed running
`sleep960`. It completed successfully. Inngest run `01M322WVTEMF5WW1FX8N8MQCX1` recorded
a completed `executor.step` from 13:36:12.876 to 13:52:36.098: **983.222170 seconds**.
The deployment applied at 13:52:36.071; public HTTPS health returned 200. This proves
one real quiet request longer than 15 minutes completed through the private route.

Inngest 1.45.1 has a two-hour executor request ceiling. Node's ongoing-response socket
timeout is disabled; request/header limits concern receiving the request.
Core defaults allow 10 minutes queue wait, 30 minutes active build, and bounded
cleanup (up to 70 seconds daemon teardown), plus source acquisition, multiple serial
builds, transfer, and deployment. **Aggregate timeout limitation:** three serial builds each using the full
10-minute queue allowance and 30-minute active-build allowance consume the entire
120-minute HTTP ceiling before source acquisition, cleanup, image transfer, or
deployment. Worst-case multi-service duration is therefore unsupported and
unqualified by this run. Only the measured single-service path is live-qualified;
this is not full timeout-budget acceptance for arbitrary deployment sizes.

## Preservation, cleanup and remaining scope

Before qualification, the existing `mechanical-platypus-production` environment's
authored-intent fingerprint was `4c0f545379f1edb7410529aebb78d33b`, and its latest
Applied attempt was `ee54c378-44f3-4627-a7cf-3e45161fb97a`. The five existing nginx
container IDs were `984cb25bd936`, `f508317fab5b`, `3053962b5e3b`, `3255feb27136`,
and `9ca4340136c2`; ingress was `d9babc0fe62f`, Corrosion `3c9d1a6d3b0c`.
Final checks confirmed the same authored-intent fingerprint, latest Applied
attempt, and all seven original container IDs. The fixture service was deleted
through the normal domain commands and its container is absent. The isolated
project and attempt history remain as qualification evidence. The only additional
running container is Ployz-managed `ployz-unregistry`; no host-wide Docker cache
pruning was performed. Qualification scripts and the temporary manifest were
removed from Railway after confirming no active fixture attempt.

Distinct builder/destination delivery belongs to the existing multi-Machine
contract harness; this live run used one VPS and does not independently prove
multi-Machine transfer. Full repository check results are recorded by the parent
implementation workflow. The cancellation fix changes the SDK structured outcome to cancelled only when
cancellation was requested and the remote result confirms termination; Unknown
remains Unknown. The real retest observed sleep 961 PID 20714 before requesting
Cancel, then confirmed the process and BuildKit container were gone, durable
status was cancelled, and the previous application still returned health 200.
No application execution followed cancellation. The original failed attempt is
retained unchanged as evidence of the defect.


## Repository checks

The implementation coordinator verified the following on the reviewed branch:

- `cargo test --locked -p ployz-core -p ployz-build -p ployz --lib --tests` passed;
  the affected CLI library/integration suite was rerun after review fixes.
- Four actual Node → native SDK → RPC contracts passed, including successful
  same-Machine and distinct build-only/runtime-only Machine variants, zero
  eligible builders before upload, failed versus unknown outcomes, and quiet
  cancellation retaining stage/work evidence. This is the distinct-Machine
  evidence; it is separate from the single-VPS hosted run.
- Thirteen real PostgreSQL runtime-persistence tests passed, including cancelled
  terminalization. Final `pnpm pr:check` on `74952ac7` passed all stages:
  195 files, 890 tests. All four review axes passed on that candidate;
  [CI run35608018717](https://github.com/getployz/ployz2/actions/runs/35608018717)
  succeeded.

Existing privileged Docker Compose suites remained ignored; this report does
not claim every privileged test executed or sum nested subprocess test counts.
