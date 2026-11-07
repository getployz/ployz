import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import type { Signed } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { findService } from "#/modules/config-store/store-lookup.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { actService } from "#/modules/operations/operations.server";
import { requireAccess } from "#/modules/organization/access.server";
import { SERVICE_ACT_VERBS } from "#/modules/service-act/service-act";
import { environmentEnforces, latestServiceAct } from "#/modules/service-act/service-act.server";
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

const hexOf = (digits: number) => Schema.String.check(Schema.isPattern(new RegExp(`^[0-9a-f]{${digits}}$`)));
const Base64Url = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{1,8192}$/));
const SignedBody = Schema.String.check(Schema.isMaxLength(4096));

/** A browser's approval as `ployz_core::authority::Signed` travels. Servers check every byte; Cloud only bounds its size. */
const ApprovalInput = Schema.Struct({
  body: SignedBody,
  signatures: Schema.Array(Schema.Struct({ key: hexOf(64), signature: hexOf(128) })).check(Schema.isMaxLength(4)),
  grants: Schema.optionalKey(Schema.Array(Schema.Struct({
    body: SignedBody,
    assertion: Schema.Struct({ credential_id: Base64Url, authenticator_data: Base64Url, client_data_json: Base64Url, signature: Base64Url }),
  })).check(Schema.isMaxLength(4))),
});

const ActServiceInput = Schema.Struct({
  ...ServiceSubject.fields,
  verb: Schema.Literals(SERVICE_ACT_VERBS),
  requestId: Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{8,64}$/)),
  /** One approval per container ID, from the browser's passkey session. */
  approvals: Schema.optionalKey(Schema.Record(hexOf(64), ApprovalInput)),
});

export const actServiceServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ActServiceInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const caller = yield* dashboardCaller(getRequest().headers, data.organizationSlug);
    const found = yield* findService(caller.organization.id, data.environmentId, data.service);
    if (found === null) return yield* new NotFound({ message: "The service was not found." });
    if (data.verb === "restart" && Object.keys(data.approvals ?? {}).length === 0 && (yield* environmentEnforces(caller.organization.id, found.namespace))) {
      return yield* new Conflict({ message: "Its servers need your passkey's approval. Reload the page and restart again.", userFacing: true });
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
