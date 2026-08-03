# Stripe API versioning and backward compatibility

Research snapshot: **2026-07-21**. Sources are limited to Stripe's official
documentation, support material, and engineering blog. This note describes
Stripe's published behavior; it does not assume that an undocumented retention
promise exists.

## Findings

### Stripe pins the contract, not the deployed service

Stripe's API service remains current while each request is interpreted and
rendered according to a selected API version. For an unversioned request, the
account's default API version applies. Stripe says this default is set when the
account makes its first API request. A caller can override it for an individual
request with the `Stripe-Version` header; organization API keys are an exception
and require that header on every request
([API upgrades](https://docs.stripe.com/upgrades),
[API versioning](https://docs.stripe.com/api/versioning),
[SDK versioning and support](https://docs.stripe.com/sdks/versioning)).

Changing the account default affects calls that omit `Stripe-Version`, Stripe.js
object shapes, unpinned webhook destinations, and some Stripe-initiated behavior
such as automated Billing operations. Explicitly versioned calls and webhook
endpoints remain on their selected versions
([API upgrades](https://docs.stripe.com/upgrades)). For Connect, a platform
request made on behalf of a connected account uses the platform's version when
the request does not specify one; the connected account's own default does not
change the response contract for that platform request
([API upgrades](https://docs.stripe.com/upgrades)).

Stripe's 2017 engineering description explains the implementation model behind
this boundary: it said Stripe first produced a resource in the current shape,
selected a target version from the request header, authorized OAuth application,
or pinned account, then applied ordered version-change modules backwards to the
target shape. Those modules kept most historical behavior outside current core
code paths. Some semantic changes could not be represented as pure response
transformations and required explicit side-effect-aware checks. This is a
historical engineering description, not a current public implementation
contract
([Stripe engineering: API versioning](https://stripe.com/blog/api-versioning)).

### Release names separate compatible increments from breaking epochs

Since `2024-09-30.acacia`, Stripe publishes a dated API version every month.
Monthly versions within one named release contain only backward-compatible
changes. Approximately twice a year, a newly named release begins with breaking
changes. Stripe also reserves the possibility of an out-of-cycle breaking
release, which would receive a new name
([SDK versioning and support](https://docs.stripe.com/sdks/versioning),
[new API release process](https://stripe.com/blog/introducing-stripes-new-api-release-process)).

Stripe classifies these changes as backward-compatible:

- adding resources;
- adding optional request parameters;
- adding response properties;
- reordering response properties;
- changing the length or format of opaque strings, IDs, error messages, and
  other human-readable strings; and
- adding event types.

The policy consequently requires consumers to tolerate unknown response fields,
new event types, and changes to strings that are not documented enums. A named
major release may instead remove, rename, or change existing contract elements
and can require integration changes
([API upgrades](https://docs.stripe.com/upgrades),
[API versioning](https://docs.stripe.com/api/versioning)).

### SDK versions carry an API-version choice

Stripe SDKs use semantic versioning. Stripe expects a new SDK minor version for
each backward-compatible monthly API version and a new SDK major version for
each twice-yearly breaking API release. An SDK can exceptionally need a major
version during a monthly API release when the SDK itself must make a breaking
change. Each server SDK release is associated with the API version current when
that SDK ships
([SDK versioning and support](https://docs.stripe.com/sdks/versioning),
[new API release process](https://stripe.com/blog/introducing-stripes-new-api-release-process)).

The exact override behavior depends on language and SDK generation:

- Current Ruby, Python, PHP, and Node generations pin requests to the API version
  associated with the library release. They permit an override in code, but
  Stripe warns that overriding Node can make its TypeScript types inaccurate.
- Strongly typed Java, Go, and .NET SDKs align requests with their release-time
  API version; Stripe directs users who need another API version to upgrade or
  downgrade the SDK.
- Older generations of several dynamic-language SDKs used the account default
  rather than a library-pinned API version.

These language-specific rules are documented in Stripe's
[API versioning reference](https://docs.stripe.com/api/versioning). Stripe also
publishes migration guides and mappings between SDK major versions and API
versions. New features and bug fixes go only to the latest SDK major. Older
major packages remain available but receive no further updates
([SDK versioning and support](https://docs.stripe.com/sdks/versioning)). Thus an
old API contract and an old SDK's maintenance status are separate promises.

### Webhook destinations are independently versioned

A webhook endpoint can have an explicit API version or inherit the account
default. The selected version controls the shape in which events are rendered
for that endpoint. Stripe recommends matching the webhook endpoint version to
the SDK's pinned API version, especially for statically typed SDKs, so event
deserialization uses the expected shape
([webhook versioning](https://docs.stripe.com/webhooks/versioning),
[Webhook Endpoint object](https://docs.stripe.com/api/webhook_endpoints/object)).

A per-request `Stripe-Version` override does not select the version of the
resulting snapshot event. Event data uses the account or endpoint event contract,
and the structure of an existing event is immutable: later account upgrades and
retrieval through a newer API version do not rewrite it
([receive webhook events](https://docs.stripe.com/webhooks#api-versioning)). This
means compatibility cannot be implemented solely by translating synchronous
request and response payloads; retained asynchronous facts preserve their
original schema.

For a breaking webhook upgrade, Stripe documents a parallel-endpoint procedure:
create a second endpoint at the new version, temporarily deliver to both old and
new endpoints, switch which version the application processes, monitor it, and
retain failed deliveries on the old path so they can be retried if the code is
reverted. Only after success does the operator disable the old endpoint
([webhook versioning](https://docs.stripe.com/webhooks/versioning)).

### Workbench makes versions visible and upgrades reversible for a short window

Workbench shows the account default and the API versions observed in recent
requests, permits filtering request logs by API version, and performs account
default upgrades. Stripe recommends exercising a candidate version first by
sending `Stripe-Version` in both test and live environments rather than changing
the default immediately
([Workbench overview](https://docs.stripe.com/workbench/overview),
[API upgrades](https://docs.stripe.com/upgrades)).

After an account default upgrade, Workbench permits rollback to the immediately
previous version for **72 hours**. Stripe says failed webhooks that used the new
shape are retried using the old shape after rollback
([API upgrades](https://docs.stripe.com/upgrades)). The separately documented
parallel-endpoint webhook procedure provides a longer operator-controlled
migration technique, but Stripe does not describe it as an automatic rollback
window
([webhook versioning](https://docs.stripe.com/webhooks/versioning)).

## Support and retirement limits

The current public API-versioning and SDK-support pages do **not** state a fixed
one-year, three-year, or indefinite support period for a pinned GA API version.
They also do not publish a general API-version retirement schedule. Therefore,
Stripe's behavior should not be cited as a contractual time-based compatibility
guarantee without separate terms or confirmation from Stripe.

Stripe's 2017 engineering article reported that Stripe had maintained every API
version since 2011, while also saying it expected eventually to retire older
versions. That statement is useful historical evidence for the architecture's
longevity, but it is neither a present duration commitment nor a promise never to
retire a version
([Stripe engineering: API versioning](https://stripe.com/blog/api-versioning)).

The explicit **1–2 year** windows on Stripe's support page concern end-of-life
*language runtime versions*, not API contract versions. Likewise, old SDK majors
remaining downloadable but frozen is not continued maintenance or security
support
([SDK versioning and support](https://docs.stripe.com/sdks/versioning)).

## Short implications for Ployz

The closest Stripe analogy to always-current Cloud serving independently aged
Cores is a target protocol version selected at the connection/request boundary,
with compatibility transformations isolated from Cloud's current domain model.
Stripe's example supports several narrower lessons:

- record the selected protocol explicitly instead of inferring it from software
  age;
- distinguish additive monthly evolution from named breaking epochs;
- require old consumers to tolerate additive fields and variants;
- version asynchronous testimony independently and preserve the schema identity
  of already-recorded facts;
- test a candidate contract before changing the default and provide a bounded,
  explicit rollback mechanism; and
- publish Ployz's own duration and retirement policy, because Stripe's public
  materials do not supply a time guarantee that Ployz can copy.

Stripe's historical adapter model is evidence for reshaping current service data
at the compatibility boundary. It is not evidence that every behavioral change
can be reduced to data reshaping: Stripe explicitly identified side-effecting
changes as exceptions, and its immutable event shapes require dedicated
asynchronous-version handling.
