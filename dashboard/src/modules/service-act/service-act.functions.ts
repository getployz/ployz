import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { findService } from "#/modules/config-store/store-lookup.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { actService } from "#/modules/operations/operations.server";
import { requireAccess } from "#/modules/organization/access.server";
import { SERVICE_ACT_VERBS } from "#/modules/service-act/service-act";
import { latestServiceAct } from "#/modules/service-act/service-act.server";
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
    const { organization } = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return yield* latestServiceAct(organization.id, { environment: data.environmentId, service: data.service });
  })));

const ActServiceInput = Schema.Struct({
  ...ServiceSubject.fields,
  verb: Schema.Literals(SERVICE_ACT_VERBS),
  requestId: Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{8,64}$/)),
});

/** Restart, start or stop a Service on every Server it runs on, through the same operation the agent and CLI ask for. */
export const actServiceServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ActServiceInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const caller = yield* dashboardCaller(getRequest().headers, data.organizationSlug);
    const found = yield* findService(caller.organization.id, data.environmentId, data.service);
    if (found === null) return yield* new NotFound({ message: "The service was not found." });
    const tool = `service_${data.verb}` as const;
    const body = { environmentId: data.environmentId, service: data.service, only: null };
    const admission = yield* actService(
      caller,
      { ...body, ...found, verb: data.verb, digest: bodyDigest(tool, body) },
      { key: data.requestId, approvalId: null },
    );
    if (!admission.ok) return yield* new Conflict({ message: admission.refusal.message, userFacing: true });
    return { id: admission.id };
  })));
