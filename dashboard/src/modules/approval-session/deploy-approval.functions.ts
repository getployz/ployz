import { createServerFn } from "@tanstack/react-start";
import { deployApprovalBody, type DeployTerms, type PlanQuery, RpcError } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { callStoreAsMember } from "#/modules/config-store/config-store.server";
import { EnvironmentRef, environmentOf } from "#/modules/config-store/store.contract";
import { storeVersion } from "#/modules/volume-placement/volume-placement";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const DeployTermsInput = Schema.Struct({
  cluster: Schema.String,
  operation: Schema.String,
  machines: Schema.Array(Schema.String),
  issued_at: Schema.Number,
  expires_at: Schema.Number,
  signer: Schema.Struct({ browser: Schema.String }),
});

const DraftInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  environment: EnvironmentRef,
  version: Schema.NonEmptyString,
  terms: DeployTermsInput,
});

/** The Store's own words for a review that moved: a Deploy admitted at `version` would be refused the same way. */
const STALE = "The Environment changed after this review. Review the latest changes and try again";

/** Whether `version`, as a Deploy carries it, names the plan's, as the Store reads it: itself, or itself with what a refusal added after a colon. */
const names = (version: string, plan: string) => {
  const sent = storeVersion(version);
  return sent === plan || sent.startsWith(`${plan}:`);
};

/**
 * The body of a Deploy's approval for the browser to sign: the Environment's plan at the version the person reviewed,
 * lowered as the runner will, with each Service's digest taken by `deployApprovalBody`, under the browser's terms.
 * Refused, as the Store refuses, for a review that moved, a Service built from source or an image named by a tag.
 */
export const draftDeployApprovalServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(DraftInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const query: { query: "plan" } & PlanQuery = { query: "plan", environment: environmentOf(data.environment), services: [], target: true };
    const read = yield* callStoreAsMember(context.actor, data.organizationSlug, { operation: "read", query });
    if (!read.ok) return read;
    const { view: _, ...plan } = read.value;
    if (!names(data.version, plan.version)) return { ok: false as const, refusal: { code: "conflict", message: STALE, details: null } };
    return yield* Effect.try({
      // SAFETY: the schema bounds the terms' shape; `deployApprovalBody` parses them and refuses the rest.
      try: () => ({ ok: true as const, body: deployApprovalBody(plan, data.terms as DeployTerms).body }),
      catch: (cause) => cause,
    }).pipe(Effect.catch((cause) => cause instanceof RpcError
      ? Effect.succeed({ ok: false as const, refusal: { code: cause.code, message: cause.message, details: null } })
      : Effect.die(cause)));
  })));
