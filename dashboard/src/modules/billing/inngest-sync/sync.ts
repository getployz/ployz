import { Effect, Option, Schema } from "effect";
import { organizationBillingSyncRequested } from "#/modules/billing/billing.events";
import { eventData } from "#/modules/inngest/define-event";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";
import { ManagedSubscriptionSnapshot } from "#/modules/billing/billing";
import {
  getActiveManagedSubscriptionSnapshot,
  persistOrganizationBillingStateSnapshot,
  syncSubscriptionUnits,
} from "#/modules/billing/billing.server";
import { listOrganizationIds } from "#/modules/organization/organization-state.server";
import type { Polar } from "#/modules/billing/polar-provider.server";
import type { Database } from "#/server/database.server";
import type { PolarConfiguration } from "#/server/config.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";

export const SYNC_ORGANIZATION_BILLING_STATE_SINGLETON = {
  key: "event.data.organizationId",
  mode: "cancel",
} as const;

export type BillingSyncStepTools = Pick<
  PloyzStepTools,
  "run" | "sendEvent"
>;

export type BillingEffectRunner = EffectRunner<Database | Polar | SecretEncryption>;

const OptionalSourceUpdatedAt = Schema.NullOr(Schema.DateFromString);
/** The snapshot as a step returns it: Inngest keeps a step's result as JSON, so the period end travels as a string. */
const DurableManagedSubscriptionSnapshot = Schema.Struct({
  ...ManagedSubscriptionSnapshot.fields,
  currentPeriodEnd: Schema.NullOr(Schema.DateFromString),
});

export async function executeSyncOrganizationBillingState(
  {
    event,
    step,
  }: {
    event: { data: unknown };
    step: BillingSyncStepTools;
  },
  runEffect: BillingEffectRunner,
) {
  const requested = eventData(organizationBillingSyncRequested)(event);
  if (Option.isNone(requested)) {
    return {
      organizationId: null,
      hasActiveSubscription: false,
      skipped: true,
    };
  }
  const { organizationId } = requested.value;

  const sourceUpdatedAtIso = await step.run(
    "normalize-source-updated-at",
    () =>
      runEffect(
        Schema.decodeUnknownEffect(OptionalSourceUpdatedAt)(
          requested.value.sourceUpdatedAt ?? null,
        ).pipe(Effect.map((value) => value?.toISOString() ?? null)),
      ),
  );

  const serializedSnapshot = await step.run("fetch-billing-state", () =>
    runEffect(
      Effect.flatMap(
        getActiveManagedSubscriptionSnapshot(organizationId),
        Schema.encodeEffect(DurableManagedSubscriptionSnapshot),
      ),
    ),
  );

  const persistedSnapshot = await step.run("persist-billing-state", () =>
    runEffect(
      Effect.gen(function* () {
        const snapshot = yield* Schema.decodeUnknownEffect(
          DurableManagedSubscriptionSnapshot,
        )(serializedSnapshot);
        yield* persistOrganizationBillingStateSnapshot(
          organizationId,
          snapshot,
          sourceUpdatedAtIso === null ? null : new Date(sourceUpdatedAtIso),
        );
        return snapshot;
      }),
    ),
  );

  if (persistedSnapshot.hasActiveSubscription) {
    await step.run("sync-subscription-units", () =>
      runEffect(
        syncSubscriptionUnits(organizationId).pipe(
          Effect.map((change) => change.kind),
        ),
      ),
    );
  }

  return {
    organizationId,
    hasActiveSubscription: persistedSnapshot.hasActiveSubscription,
  };
}

export async function executeScheduleNightlyBillingReconcile(
  { step }: { step: BillingSyncStepTools },
  runEffect: BillingEffectRunner,
) {
  const organizationIds = await step.run("list-organization-ids", () =>
    runEffect(listOrganizationIds()),
  );

  await step.sendEvent(
    "request-billing-syncs",
    organizationIds.map((organizationId) =>
      organizationBillingSyncRequested.create({
        organizationId,
        reason: "nightly-reconcile",
      }),
    ),
  );

  return { organizationCount: organizationIds.length };
}

export const createSyncOrganizationBillingStateFunction = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "sync-organization-billing-state",
    retries: 3,
    triggers: [{ event: organizationBillingSyncRequested.eventType }],
    singleton: SYNC_ORGANIZATION_BILLING_STATE_SINGLETON,
    concurrency: [{ key: "event.data.organizationId", limit: 1 }],
  },
  async ({ event, step }) =>
    executeSyncOrganizationBillingState(
      { event, step },
      runInngestEffect,
    ),
  );

export const createScheduleNightlyBillingReconcile = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "schedule-nightly-billing-reconcile",
      retries: 3,
      triggers: [{ cron: "TZ=UTC 0 2 * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) =>
      executeScheduleNightlyBillingReconcile(
        { step },
        runInngestEffect,
      ),
  );

/** A Self-hosted Cloud has no Polar, so billing sync is never registered. */
export function billingInngestFunctions(
  inngest: PloyzInngest,
  billingMode: PolarConfiguration["mode"],
) {
  return billingMode === "hosted"
    ? [
        createSyncOrganizationBillingStateFunction(inngest),
        createScheduleNightlyBillingReconcile(inngest),
      ]
    : [];
}
