import { describe, expect, it } from "vitest";
import { billingSyncsFromCustomerStatePayload, billingSyncsFromSubscriptionPayload } from "#/modules/billing/polar-events";

describe("Polar webhook billing syncs", () => {
  it("creates billing sync events from webhook reference ids", () => {
    const subscriptionEvents =
      billingSyncsFromSubscriptionPayload(
      {
        type: "subscription.updated",
        timestamp: "2026-03-27T00:00:00Z",
        data: {
          metadata: {
            referenceId: "org-1",
          },
        },
      } as never,
    );

    const customerEvents =
      billingSyncsFromCustomerStatePayload(
      {
        type: "customer.state_changed",
        timestamp: "2026-03-27T01:00:00Z",
        data: {
          active_subscriptions: [
            {
              metadata: {
                referenceId: "org-1",
              },
            },
            {
              metadata: {
                referenceId: "org-2",
              },
            },
          ],
        },
      } as never,
    );

    expect(subscriptionEvents).toEqual([
      {
        name: "billing/organization-sync.requested",
        data: {
          organizationId: "org-1",
          reason: "subscription.updated",
          sourceUpdatedAt: "2026-03-27T00:00:00.000Z",
        },
      },
    ]);
    expect(customerEvents).toEqual([
      {
        name: "billing/organization-sync.requested",
        data: {
          organizationId: "org-1",
          reason: "customer.state_changed",
          sourceUpdatedAt: "2026-03-27T01:00:00.000Z",
        },
      },
      {
        name: "billing/organization-sync.requested",
        data: {
          organizationId: "org-2",
          reason: "customer.state_changed",
          sourceUpdatedAt: "2026-03-27T01:00:00.000Z",
        },
      },
    ]);
  });
});
