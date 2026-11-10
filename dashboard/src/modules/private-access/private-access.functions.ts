import type { PrivateAccessOn } from "@ployz/sdk";
import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireOperator } from "#/modules/ingress/doors.access.server";
import { requireAccess } from "#/modules/organization/access.server";
import { ApprovePrivateAccessInput, SetPrivateAccessInput } from "#/modules/private-access/private-access";
import { approvePrivateAccess, observePrivateAccess, setPrivateAccess } from "#/modules/private-access/private-access.server";
import { Validation } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Slug = Schema.Struct({ organizationSlug: Schema.NonEmptyString });

// SAFETY: the schema bounds each field's shape; Servers parse and verify the rest.
const relayed = (on: typeof ApprovePrivateAccessInput.Type["on"]) => on as PrivateAccessOn;

export const observePrivateAccessServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Slug))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireAccess(context.actor, data.organizationSlug, "servers:read");
    return yield* observePrivateAccess(organization.id);
  })));

export const setPrivateAccessServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetPrivateAccessInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireOperator(context.actor, data.organizationSlug);
    if (data.to === "off") return yield* setPrivateAccess(organization.id, null);
    if (data.on === undefined) return yield* new Validation({ userFacing: true, message: "Paste a Tailscale auth key." });
    return yield* setPrivateAccess(organization.id, relayed(data.on));
  })));

export const approvePrivateAccessServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ApprovePrivateAccessInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    yield* requireOperator(context.actor, data.organizationSlug);
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    return yield* approvePrivateAccess({ ...access, via: "dashboard" }, { id: data.id, digest: data.digest, on: relayed(data.on) });
  })));
