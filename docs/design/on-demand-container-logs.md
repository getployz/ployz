# On-demand container logs

Status: implementation authorized; no prototype gate. This document records the accepted scope.

Cloud's Deployment, Service and Environment pages share one viewer. Deployment logs combine concise lifecycle events with stdout/stderr from application and pre-deploy hook containers. Build logs remain separate. Deployment completion does not stop following container output.

Application output stays in Docker. The viewer initially reads 200 records per container, follows while open, and offers Load older. A viewer-local TanStack DB collection owns loaded records; rendering is virtualized, with no additional browser buffer cap. Search covers loaded output; service and server filters narrow that output.

The existing Corrosion runtime watch discovers matching containers, including retained stopped containers and hooks. No polling discovery process or persistent log collector is added. Containers carry the stable Cloud service ID as a user label and the creating deployment attempt ID as execution metadata; deployment identity does not alter authored service configuration. The existing Project Name identifies the environment namespace. Machine ID and Container ID identify each source.

## Transport and history

Preserve the existing ContainerLogs RPC for tails and following. Add an independently advertised ContainerLogHistory operation for finite history reads: Container ID, decimal nanosecond timestamp boundary, and a limit of 1–1000 older records. The daemon expands local Docker tails and returns the nearest older records plus the inclusive timestamp boundary. Timestamp groups remain intact, so a page can exceed its requested count.

Server RPCs must remain supported after release. Do not expose Cloud identities or speculative Docker record cursors in this transport. The SDK handles filters, source discovery, multiplexing and viewer record identities. History positions are per source because initial tails cover different time ranges. Loading history leaves the live stream open.

Docker does not provide permanent record IDs or snapshots. Pagination is best effort across rotation, clock regressions and identical timestamp groups; an opaque token would not strengthen those guarantees. Boundary reads replace a potentially partial timestamp group in the viewer. Identical messages remain distinct. Removed containers and rotated output are unavailable; this is not retained logging.

Follow reconnects after clean EOF when Corrosion observes the container running, rereading from the last timestamp's second and suppressing already-delivered boundary records. Source errors are reported separately; one failed source does not stop the others. Closing the viewer cancels its reads.

Paid ClickHouse storage, continuous collection, 30-day retention and billing are deferred. No backend plugin framework is needed for this release.

## Verification

Use daemon history selection tests, the existing real-Docker RPC integration test, SDK stream tests and Cloud collection/authorization seams. Run the dashboard's final PR gate and four independent review axes. No separate exploratory prototype is required.

## Measured read cost — 2026-09-22

Benchmarked Docker 29.1.3 on the development VM (AMD Ryzen 9 5950X, x86-64), through the local Unix socket. Synthetic stdout records contained a sequence number and 240 padding characters (251 payload bytes, approximately 290 bytes with Docker timestamps/framing). Three measured runs per case; medians below. Recent, warm log files; this is not a production VPS, cold-disk, network, or end-to-end Cloud benchmark.

| Read strategy | History / scenario | Median |
| --- | --- | --- |
| Scan history before cutoff, retain last 200 | 10,000 lines, cutoff near end | 62 ms |
| Same | 100,000 lines | 489 ms |
| Same | 1,000,000 lines | 4,973 ms |
| Expanding native tails, filter locally | 1,000,000 lines, preceding recent 200 | 3.6 ms json-file; 3.0 ms local |
| Same | 100,000 newer lines beyond cutoff | 1,171 ms json-file; 1,351 ms local |
| Same | Concurrent writer targeting 10,000 lines/sec, cutoff captured two seconds earlier | 546 ms json-file |

The expanding read starts at tail=400, filters before the requested timestamp locally, and doubles only if it has not found 200 eligible records and Docker returned a full tail. Each query returns only the selected 200. Static and concurrent-write tests asserted exact sequence-number ranges. This tests monotonic, distinct timestamps, not timestamp ties, clock regressions, rotation, or browser/live-stream merging.

Native tail=200 with an older until bound returned zero records despite retained older output, confirming that it cannot implement a previous-page request.

Recommendation: preserve the Load older UI and implement the extra read on the Machine, not in Cloud. Recent history required only 116 KB of local Docker response versus approximately 290 MB for the million-line full scan. Returning 200 records requires roughly 58 KB before protocol differences; this network reduction is calculated, not measured across iroh. No application-log persistence is introduced. The current reader sends all requested Docker records outward, so local selection needs a deliberate RPC/daemon change.

This supersedes the earlier recommendation to restart/replace the viewer's live stream for every history expansion: perform finite history reads separately, retaining the current live stream. Record identity, equal-timestamp boundaries, and merging are covered by implementation tests. The benchmark supports the cost decision, not a claim of exact pagination under every Docker retention condition.

Runnable benchmark artifacts: `/tmp/ployz-log-bench/bench.py`, `/tmp/ployz-log-bench/adaptive.py`; raw results: `results.json` and `adaptive-results.json` in the same directory. Benchmark containers were removed.
