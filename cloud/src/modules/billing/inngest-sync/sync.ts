import { Effect, Option, Schema } from "effect";
import {
  createOrganizationBillingSyncRequestedEvent,
  organizationBillingSyncRequestedEventType,
} from "#/modules/inngest/events";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { runInngestEffect } from "#/server/run.server";
import {
  BillingPlan,
  persistableManagedSubscriptionSnapshot,
} from "#/modules/billing/billing";
import {
  getActiveManagedSubscriptionSnapshot,
  persistOrganizationBillingStateSnapshot,
} from "#/modules/billing/billing.server";
import { listOrganizationIds } from "#/modules/environment-design/workspace-repository.server";
import type { Polar } from "#/modules/billing/polar-provider.server";
import type { Database } from "#/server/database.server";

export const SYNC_ORGANIZATION_BILLING_STATE_SINGLETON = {
  key: "event.data.organizationId",
  mode: "cancel",
} as const;

export type BillingSyncStepTools = Pick<
  PloyzStepTools,
  "run" | "sendEvent"
>;

export type BillingEffectRunner = <A, E extends Error>(
  effect: Effect.Effect<A, E, Database | Polar>,
) => Promise<A>;

const OrganizationId = Schema.Trim.check(Schema.isMinLength(1));
const OptionalSourceUpdatedAt = Schema.NullOr(Schema.DateFromString);
const OrganizationBillingSyncEventData = Schema.Struct({
  organizationId: Schema.optionalKey(Schema.Unknown),
  sourceUpdatedAt: Schema.optionalKey(Schema.Unknown),
});
const DurableManagedSubscriptionSnapshot = Schema.Struct({
  activeSubscriptionId: Schema.NullOr(Schema.String),
  currentPlan: Schema.NullOr(BillingPlan),
  productId: Schema.NullOr(Schema.String),
  amount: Schema.NullOr(Schema.Finite),
  currency: Schema.NullOr(Schema.String),
  currentPeriodStart: Schema.NullOr(Schema.DateFromString),
  currentPeriodEnd: Schema.NullOr(Schema.DateFromString),
  hasActiveSubscription: Schema.Boolean,
  hasUnknownActiveProduct: Schema.Boolean,
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
  const decodedEventData = Schema.decodeUnknownOption(
    OrganizationBillingSyncEventData,
  )(event.data, { onExcessProperty: "preserve" });
  const eventData = Option.isSome(decodedEventData)
    ? decodedEventData.value
    : {};
  const organizationId = await step.run("normalize-organization-id", () => {
    const decoded = Schema.decodeUnknownOption(OrganizationId)(
      eventData.organizationId,
    );
    return Option.isSome(decoded) ? decoded.value : null;
  });

  if (organizationId === null) {
    return {
      organizationId: null,
      hasActiveSubscription: false,
      currentPlan: null,
      skipped: true,
    };
  }

  const sourceUpdatedAtIso = await step.run(
    "normalize-source-updated-at",
    () =>
      runEffect(
        Schema.decodeUnknownEffect(OptionalSourceUpdatedAt)(
          eventData.sourceUpdatedAt ?? null,
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
          persistableManagedSubscriptionSnapshot(snapshot),
          sourceUpdatedAtIso === null ? null : new Date(sourceUpdatedAtIso),
        );
        return snapshot;
      }),
    ),
  );

  return {
    organizationId,
    hasActiveSubscription: persistedSnapshot.hasActiveSubscription,
    currentPlan: persistedSnapshot.currentPlan,
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
      createOrganizationBillingSyncRequestedEvent({
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
    triggers: [{ event: organizationBillingSyncRequestedEventType }],
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
