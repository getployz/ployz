import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import type { Signed } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { ApprovalsInput } from "#/modules/approval-session/approvals-input";
import { findService } from "#/modules/config-store/store-lookup.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { actService, asksProtector } from "#/modules/operations/operations.server";
import { requireAccess } from "#/modules/organization/access.server";
import { SERVICE_ACT_VERBS } from "#/modules/service-act/service-act";
import { latestServiceAct, serviceEnforces, strandingStop } from "#/modules/service-act/service-act.server";
import { Conflict, NotFound } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const ServiceSubject = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  environmentId: Schema.NonEmptyString,
  service: Schema.NonEmptyString,
});

export const latestServiceActServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ServiceSubject))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireAccess(context.actor, data.organizationSlug, "project:read");
    return yield* latestServiceAct(organization.id, { environment: data.environmentId, service: data.service });
  })));

const ActServiceInput = Schema.Struct({
  ...ServiceSubject.fields,
  verb: Schema.Literals(SERVICE_ACT_VERBS),
  requestId: Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{8,64}$/)),
  approvals: Schema.optional(ApprovalsInput),
});

export const actServiceServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ActServiceInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const caller = yield* dashboardCaller(getRequest().headers, data.organizationSlug);
    const found = yield* findService(caller.organization.id, data.environmentId, data.service);
    if (found === null) return yield* new NotFound({ message: "The service was not found." });
    const stranding = data.verb === "stop" ? yield* strandingStop(caller.organization.id, found) : null;
    if (stranding !== null) return yield* new Conflict({ message: stranding, userFacing: true });
    // A press an owner must approve carries no approvals: the owner's browser makes them when it approves.
    const unapproved = Object.keys(data.approvals ?? {}).length === 0;
    if (unapproved && !(yield* asksProtector(caller, { ...found, environmentId: data.environmentId, verb: data.verb })) && (yield* serviceEnforces(caller.organization.id, found))) {
      return yield* new Conflict({ message: "Reload the page and try again.", userFacing: true });
    }
    const tool = `service_${data.verb}` as const;
    const body = { environmentId: data.environmentId, service: data.service, only: null };
    const admission = yield* actService(
      caller,
      // SAFETY: the schema bounds each field's shape; Servers parse and verify the rest.
      { ...body, ...found, verb: data.verb, digest: bodyDigest(tool, body), approvals: data.approvals as Readonly<Record<string, Signed>> | undefined },
      { key: data.requestId, approvalId: null },
    );
    // A stop or restart in a Protected Environment its asker can't protect waits for someone who can.
    if (!admission.ok && admission.refusal.code === "approval_required") return { requested: true as const };
    if (!admission.ok) return yield* new Conflict({ message: admission.refusal.message, userFacing: true });
    return { id: admission.id };
  })));
