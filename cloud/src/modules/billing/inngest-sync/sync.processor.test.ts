import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import { organizationBillingSyncRequestedEventType } from "#/modules/inngest/events";
import * as workspaceRepository from "#/modules/environment-design/workspace-repository.server";
import {
  createScheduleNightlyBillingReconcile,
  createSyncOrganizationBillingStateFunction,
} from "./sync";

useServiceFreeEffectRunner();

vi.spyOn(workspaceRepository, "listOrganizationIds").mockImplementation(() =>
  Effect.succeed(["organization-1"]),
);

describe("billing sync Inngest adapter", () => {
  it("preserves the trigger, retries, singleton, and keyed concurrency", () => {
    const fn = createSyncOrganizationBillingStateFunction(
      new Inngest({ id: "test" }),
    );

    expect(fn.opts).toEqual(
      expect.objectContaining({
        id: "sync-organization-billing-state",
        retries: 3,
        triggers: [{ event: organizationBillingSyncRequestedEventType }],
        singleton: {
          key: "event.data.organizationId",
          mode: "cancel",
        },
        concurrency: [{ key: "event.data.organizationId", limit: 1 }],
      }),
    );
  });

  it("skips invalid organization identity through the actual function", async () => {
    const output = await new InngestTestEngine({
      function: createSyncOrganizationBillingStateFunction(
        new Inngest({ id: "test" }),
      ),
      events: [
        { name: "billing/organization-sync.requested", data: {} },
      ],
    }).execute();

    expect(output.result).toEqual({
      organizationId: null,
      hasActiveSubscription: false,
      currentPlan: null,
      skipped: true,
    });
    expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
  });

  it("executes the nightly cron through its durable list and send steps", async () => {
    const fn = createScheduleNightlyBillingReconcile(
      new Inngest({ id: "test" }),
    );
    expect(fn.opts.triggers).toEqual([{ cron: "TZ=UTC 0 2 * * *" }]);

    const output = await new InngestTestEngine({
      function: fn,
      steps: [
        {
          id: "request-billing-syncs",
          handler: () => ({ ids: ["event-1"] }),
        },
      ],
    }).execute();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ organizationCount: 1 });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "list-organization-ids",
      expect.any(Function),
    );
    expect(output.ctx.step.sendEvent).toHaveBeenCalledWith(
      "request-billing-syncs",
      [
        {
          name: "billing/organization-sync.requested",
          data: {
            organizationId: "organization-1",
            reason: "nightly-reconcile",
          },
        },
      ],
    );
  });
});
