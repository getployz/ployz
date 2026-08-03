# OpenShip public usage and payer evidence

Date: 2026-08-01

## Bottom line

No current first-party public source gives a defensible count of OpenShip's
active servers, workloads, cloud workspaces, customers, or paying customers.
The strongest public lower bound is **at least one external user's remote server
and one successfully served workload**. The public evidence supports broader
awareness, downloads, and deployment attempts, but not a managed-fleet number or
revenue claim.

There is likewise no first-party public proof that a named customer—or any
customer—is currently paying OpenShip. The marketing pricing page describes who
*would* pay and on what basis, but the live public billing API currently returns
`null` prices for Pro and Team, while the checked-in billing documentation says
Cloud billing is coming soon. A private production override or private contract
could exist; public evidence cannot establish one.

## Hard public facts

### Servers and workloads

- OpenShip supports local, SSH-reachable server, and Cloud deployment targets,
  but the official architecture documentation publishes no aggregate usage or
  fleet statistics
  ([official architecture overview](https://openship.io/docs/architecture/overview),
  [runtime model](https://openship.io/docs/architecture/runtime-model)).
- Self-hosted usage is intentionally unobservable to OpenShip: the pricing page
  says Hobby has “no telemetry,” and the homepage says telemetry is off by
  default. Therefore even OpenShip itself cannot derive a complete public or
  private fleet count from ordinary self-hosted installations
  ([pricing](https://openship.io/pricing),
  [homepage](https://openship.io/)).
- The public SaaS health endpoint was live and reported `cloudMode: true` on
  2026-08-01. It exposes no project, workspace, deployment, server, or customer
  count
  ([live health endpoint](https://api.openship.io/api/health)).
- One detailed external issue supplies the only hard successful-deployment lower
  bound found. Its reporter identifies an Ubuntu 24.04 remote target with Docker
  and OpenResty, and reports that the Node-run API completed build, verification,
  container creation/start, and OpenResty routing for a Next.js app which served
  HTTP 200. This proves at least one server and one workload had been deployed
  through OpenShip; it does not prove that workload is still active, production,
  paid, or part of a multi-server fleet
  ([issue #10](https://github.com/oblien/openship/issues/10)).
- Public issues show that independent users are attempting real server and Cloud
  workflows, but do not prove successful or currently active workloads. Examples
  include a Cloud deployment refused because no free workspaces were available,
  a reported crash-looping deployed container, and an SSH/SFTP exhaustion report
  from server use
  ([Cloud deploy issue #349](https://github.com/oblien/openship/issues/349),
  [deployment issue #335](https://github.com/oblien/openship/issues/335),
  [server connection issue #291](https://github.com/oblien/openship/issues/291)).

### Billing and payer model

- The official pricing page advertises three buyer paths: self-hosted Hobby at
  $0; Cloud at $20 per active team member per month ($16 effective annually),
  plus compute and bandwidth after allowances; and Business at custom per-project
  pricing. Under this published model, Cloud team members and Business project
  owners are the intended OpenShip payers, while self-hosters pay their own
  infrastructure providers rather than OpenShip
  ([official pricing](https://openship.io/pricing)).
- The checked-in Billing API documentation says OpenShip Cloud “isn't available
  yet,” self-hosted has no billing, and the Stripe subscription, metered usage,
  top-up, and billing portal documentation will arrive before Cloud becomes a
  paid service
  ([Billing API documentation](https://github.com/oblien/openship/blob/main/apps/web/content/docs/api/billing.mdx)).
- The checked-in user billing guide independently says managed Cloud pricing and
  billing are not available yet, while self-hosted users owe OpenShip nothing and
  pay only for their own server, bandwidth, and optional third parties
  ([billing guide](https://github.com/oblien/openship/blob/main/apps/web/content/docs/guides/billing.mdx)).
- The checked-in server configuration defaults `BILLING_ENABLED` to false. In
  that state Stripe mutations fail closed with HTTP 403; the comment explicitly
  allows the SaaS deployment to turn it on privately
  ([billing configuration](https://github.com/oblien/openship/blob/main/apps/api/src/config/env.ts#L140-L159)).
- The live public plans endpoint currently returns a free tier with one workspace,
  but `price.monthly` and `price.annual` are both `null` for Pro and Team. The
  checked-in plan catalog calls those tiers “coming soon,” uses placeholder
  Stripe price IDs by default, and says pricing is intentionally not published
  there
  ([live plans endpoint](https://api.openship.io/api/billing/plans),
  [plan catalog](https://github.com/oblien/openship/blob/main/packages/core/src/constants.ts#L143-L232)).

These facts conflict with parts of the marketing pricing page. The live API and
checked-in product contract are stronger evidence of current billing readiness;
none of them disclose an actual charge, subscriber, invoice, customer name, or
revenue total.

## Adoption proxies—not fleet counts

As of 2026-08-01:

| Proxy | Observed value | What it does and does not mean |
| --- | ---: | --- |
| GitHub stars | 10,021 | Awareness/interest; not installs, servers, workloads, or customers. |
| GitHub forks | 804 | Repository copies; not running installations. |
| npm package downloads, 2026-07-01 through 2026-07-31 | 5,734 | Package fetches; includes upgrades, CI, caches, retries, and maintainers, and cannot identify active installs. |
| GitHub release installer downloads across public releases through the observation time | 4,567 | Asset downloads, summed across versions and operating systems; repeat downloads and upgrades prevent a user or install count. |
| Public issue authors | 83 unique authors across 148 non-PR issues | Evidence of an external user/contributor community; issue creation does not imply a successful deployment or payment. |

Sources: [GitHub repository API](https://api.github.com/repos/oblien/openship),
[GitHub releases API](https://api.github.com/repos/oblien/openship/releases?per_page=100),
[GitHub issues API](https://api.github.com/repos/oblien/openship/issues?state=all&per_page=100),
and the [npm downloads API](https://api.npmjs.org/downloads/point/2026-07-01:2026-07-31/openship).
The release total excludes checksum files and the separately packaged dashboard
and email-service archives; the issue-author total was deduplicated across all
pages of the official GitHub API.

The homepage also renders “Live · 247 sending” beside a mail-dashboard image.
It does not define whether 247 is live production state, sample UI data,
messages, domains, or users, so it is not usable as a workload or customer count
([homepage](https://openship.io/)).

## What remains unknown

Public first-party sources do not answer:

- how many self-hosted OpenShip control planes exist;
- how many distinct servers they target;
- how many containers, services, projects, or deployments are active;
- how many OpenShip Cloud workspaces have ever run or are running now;
- whether the “no free workspaces” response reflects real capacity exhaustion,
  a launch gate, or another policy;
- whether `BILLING_ENABLED` is true in production despite the live null prices;
- whether any Stripe charge or Business contract has been completed;
- who funds the underlying Oblien compute or the commercial relationship between
  Oblien and OpenShip; or
- customer count, ARR/MRR, revenue, churn, or paid-seat count.

A defensible quantitative answer would require a first-party fleet/status metric,
an authenticated Cloud usage report, Stripe-derived aggregate, or a company
statement naming current customers or revenue. None was found publicly as of the
date above.
