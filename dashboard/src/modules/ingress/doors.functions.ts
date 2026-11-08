import { createServerFn } from "@tanstack/react-start";
import { Effect, Redacted, Schema } from "effect";
import { listCloudflareZoneNames, setUpCloudflare } from "#/modules/cloudflare/setup.server";
import { ListCloudflareZonesInput, SetUpCloudflareInput, SwitchDoorsInput } from "#/modules/ingress/doors";
import { observeClusterIngress, readClusterIngress, requestDoorsSwitch, switchAsked } from "#/modules/ingress/doors.server";
import { requireOperator } from "#/modules/ingress/doors.access.server";
import { requireAccess } from "#/modules/organization/access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Slug = Schema.Struct({ organizationSlug: Schema.NonEmptyString });

/** Where the Cluster's names come from and the latest switch, from the database alone. */
export const readClusterIngressServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Slug))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireAccess(context.actor, data.organizationSlug, "servers:read");
    return yield* readClusterIngress(organization.id);
  })));

/** What one look at the Servers finds: who takes web traffic, whose connector runs, and the hostnames published. */
export const observeClusterIngressServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Slug))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireAccess(context.actor, data.organizationSlug, "servers:read");
    return yield* observeClusterIngress(organization.id);
  })));

/** The hostnames a switch would move, for the review before the human confirms. */
export const reviewDoorsSwitchServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SwitchDoorsInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireOperator(context.actor, data.organizationSlug);
    const asked = yield* switchAsked(organization.id, data.to);
    return { moves: asked.preview.hostnames, zone: asked.preview.zone, hostedName: asked.preview.hostedName };
  })));

/** The zones the pasted token can see. The token is used for this call and not kept. */
export const listCloudflareZonesServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ListCloudflareZonesInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    yield* requireOperator(context.actor, data.organizationSlug);
    return { zones: yield* listCloudflareZoneNames(Redacted.make(data.token)) };
  })));

/** Set Cloudflare up with the pasted token: the tunnel, its connector beside the public ports, and one wildcard record. */
export const setUpCloudflareServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetUpCloudflareInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireOperator(context.actor, data.organizationSlug);
    return yield* setUpCloudflare(organization.id, { token: Redacted.make(data.token), zone: data.zone });
  })));

/** Switch the Cluster's web traffic. The human confirmed the review the page showed. */
export const switchDoorsServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SwitchDoorsInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization, userId } = yield* requireOperator(context.actor, data.organizationSlug);
    return yield* requestDoorsSwitch({ organizationId: organization.id, userId, to: data.to });
  })));
