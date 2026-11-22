import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import * as workspaceRepository from "#/modules/organization/organization-state.server";
import {
  createScheduleNightlyBillingReconcile,
  createSyncOrganizationBillingStateFunction,
} from "./sync";

useServiceFreeEffectRunner();

vi.spyOn(workspaceRepository, "listOrganizationIds").mockImplementation(() =>
  Effect.succeed(["organization-1"]),
);

describe("billing sync Inngest adapter", () => {
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
      skipped: true,
    });
    expect(output.ctx.step.run).not.toHaveBeenCalled();
  });

  it("asks for a billing sync of every Organization each night", async () => {
    const fn = createScheduleNightlyBillingReconcile(
      new Inngest({ id: "test" }),
    );

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
    expect(output.ctx.step.sendEvent).toHaveBeenCalledWith(
      expect.any(String),
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
