import { Option, Schema } from "effect";
import { describe, expect, it } from "vitest";
import {
  createEnvironmentDeployConfirmedEvent,
  createEnvironmentDeployRequestedEvent,
  createMachineRemoveRequestedEvent,
  createGithubCheckSuiteReceivedEvent,
  createGithubCheckSuiteTransitionEvent,
  createGithubEnvironmentTriggerPersistedEvent,
  createGithubPushReceivedEvent,
  createVolumeRemoveRequestedEvent,
  createTeardownRequestedEvent,
  createOrganizationBillingSyncEventsFromCustomerStatePayload,
  createOrganizationBillingSyncEventsFromSubscriptionPayload,
  inngestEventEnvelopeFields,
  inngestFunctionFailedEnvelopeSchema,
} from "#/modules/inngest/events";

describe("Inngest events", () => {
  it("decodes complete function-failed envelopes for an owned event", () => {
    const ownedEvent = Schema.Struct({
      ...inngestEventEnvelopeFields,
      name: Schema.Literal("owned/requested"),
      data: Schema.Struct({ attemptId: Schema.String }),
    });
    const decode = Schema.decodeUnknownOption(
      inngestFunctionFailedEnvelopeSchema(ownedEvent),
    );
    const valid = decode({
      id: "failed-event-1",
      ts: 1,
      name: "inngest/function.failed",
      data: {
        function_id: "owned-function",
        run_id: "run-1",
        error: { name: "Error", message: "failed" },
        event: {
          name: "owned/requested",
          data: { attemptId: "attempt-1" },
        },
      },
    });

    expect(Option.isSome(valid)).toBe(true);
    expect(
      Option.isNone(
        decode({
          name: "inngest/function.failed",
          data: {
            function_id: " ",
            run_id: "run-1",
            error: { name: "Error", message: "failed" },
            event: {
              name: "owned/requested",
              data: { attemptId: "attempt-1" },
            },
          },
        }),
      ),
    ).toBe(true);
  });

  it("creates deterministic volume remove events from attempt id", () => {
    expect(
      createVolumeRemoveRequestedEvent({ attemptId: "attempt-1" }),
    ).toEqual({
      id: "volume-remove-attempt-1",
      name: "cloud/volume-remove.requested",
      data: { attemptId: "attempt-1" },
    });
  });

  it("creates deterministic machine remove attempts from the durable row id", () => {
    expect(
      createMachineRemoveRequestedEvent({
        attemptId: "11111111-1111-4111-8111-111111111111",
      }),
    ).toEqual({
      id: "machine-remove-11111111-1111-4111-8111-111111111111",
      name: "machine/remove.requested",
      data: { attemptId: "11111111-1111-4111-8111-111111111111" },
    });
  });

  it("creates deterministic teardown events from attempt id", () => {
    expect(
      createTeardownRequestedEvent({ attemptId: "attempt-1" }),
    ).toEqual({
      id: "teardown-attempt-1",
      name: "cloud/teardown.requested",
      data: { attemptId: "attempt-1" },
    });
  });

  it("creates deterministic environment deploy confirmed events", () => {
    expect(
      createEnvironmentDeployConfirmedEvent({
        environmentDeploymentId: "deployment-1",
      }),
    ).toEqual({
      id: "environment-deploy-confirmed-deployment-1",
      name: "environment/deploy.confirmed",
      data: { environmentDeploymentId: "deployment-1" },
    });
  });

  it("creates deterministic environment deploy requested events", () => {
    expect(
      createEnvironmentDeployRequestedEvent({
        environmentDeploymentId: "deployment-1",
        environmentId: "env-1",
      }),
    ).toEqual({
      id: "environment-deploy-requested-deployment-1",
      name: "environment/deploy.requested",
      data: {
        environmentDeploymentId: "deployment-1",
        environmentId: "env-1",
      },
    });
  });

  it("creates deterministic persisted GitHub environment-trigger events", () => {
    expect(
      createGithubEnvironmentTriggerPersistedEvent({
        triggerId: "trigger-1",
        triggerRevision: 3,
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        headSha: "a".repeat(40),
        environmentId: "env-1",
        serviceIds: ["svc-z", "svc-a", "svc-a"],
        selection: { mode: "paths", reason: "changed_paths" },
        sourceDeliveryId: "delivery-1",
        sourceReceiptSequence: 9,
      }),
    ).toEqual({
      id: "github-environment-trigger-trigger-1-revision-3",
      name: "github/environment-trigger.persisted",
      data: {
        triggerId: "trigger-1",
        triggerRevision: 3,
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        headSha: "a".repeat(40),
        environmentId: "env-1",
        serviceIds: ["svc-a", "svc-z"],
        selection: { mode: "paths", reason: "changed_paths" },
        sourceDeliveryId: "delivery-1",
        sourceReceiptSequence: 9,
      },
    });
  });

  it("creates exact-SHA check-suite transition identities from durable revisions", () => {
    const transition = {
      installationId: 17,
      repositoryId: 42,
      headSha: "b".repeat(40),
      checkSuiteId: 9001,
      status: "completed" as const,
      conclusion: "success" as const,
      sourceUpdatedAt: "2026-07-16T07:00:00.000Z",
      transitionRevision: 4,
      sourceDeliveryId: "delivery-2",
      sourceReceiptSequence: 10,
    };

    const event = createGithubCheckSuiteTransitionEvent(transition);
    expect(event).toEqual({
      id: `github-check-suite-17-42-${"b".repeat(40)}-9001-revision-4`,
      name: "github/check-suite.transitioned",
      data: transition,
    });
    expect(createGithubCheckSuiteTransitionEvent(transition)).toEqual(event);
    expect(
      createGithubCheckSuiteTransitionEvent({
        ...transition,
        transitionRevision: 5,
      }).id,
    ).not.toBe(event.id);
  });

  it("creates delivery-deduplicated GitHub ingestion events with authority keys", () => {
    expect(
      createGithubPushReceivedEvent({
        deliveryId: "delivery-push-1",
        kind: "push",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        branch: "main",
        beforeSha: "A".repeat(40),
        afterSha: "B".repeat(40),
        created: false,
        deleted: false,
        forced: false,
      }),
    ).toEqual({
      id: "delivery-push-1",
      name: "github/push.received",
      data: expect.objectContaining({
        deliveryId: "delivery-push-1",
        branchKey: "17:42:refs/heads/main",
        beforeSha: "a".repeat(40),
        afterSha: "b".repeat(40),
      }),
    });
    expect(
      createGithubCheckSuiteReceivedEvent({
        deliveryId: "delivery-suite-1",
        kind: "check_suite",
        action: "completed",
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 9001,
        headSha: "C".repeat(40),
        status: "completed",
        conclusion: "success",
        sourceUpdatedAt: "2026-07-16T08:00:00.000+01:00",
      }),
    ).toEqual({
      id: "delivery-suite-1",
      name: "github/check-suite.received",
      data: expect.objectContaining({
        deliveryId: "delivery-suite-1",
        checkSuiteKey: "17:42:9001",
        headSha: "c".repeat(40),
        sourceUpdatedAt: "2026-07-16T07:00:00.000Z",
      }),
    });
  });

  it("rejects non-canonical GitHub event authority data", () => {
    expect(() =>
      createGithubPushReceivedEvent({
        deliveryId: "delivery-push-invalid",
        kind: "push",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/tags/release",
        branch: "release",
        beforeSha: "not-a-sha",
        afterSha: "b".repeat(40),
        created: false,
        deleted: false,
        forced: false,
      }),
    ).toThrow();
  });

  it("creates billing sync events from webhook reference ids", () => {
    const subscriptionEvents =
      createOrganizationBillingSyncEventsFromSubscriptionPayload(
      {
        type: "subscription.updated",
        timestamp: new Date("2026-03-27T00:00:00.000Z"),
        data: {
          metadata: {
            referenceId: "org-1",
          },
        },
      } as never,
    );

    const customerEvents =
      createOrganizationBillingSyncEventsFromCustomerStatePayload(
      {
        type: "customer.state_changed",
        timestamp: new Date("2026-03-27T01:00:00.000Z"),
        data: {
          activeSubscriptions: [
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
