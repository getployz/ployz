import { describe, expect, it, vi } from "vitest";
import { Effect, Redacted } from "effect";
import {
  makePolarClient,
  makePolarCore,
  POLAR_API_VERSION,
  type Polar as PolarSdk,
} from "#/modules/billing/polar-api";
import {
  makePolarService,
  PolarFailure,
} from "#/modules/billing/polar-provider.server";
import { asTestDouble } from "#/lib/test-double";

type SubscriptionUpdateBody = {
  readonly units?: number;
  readonly proration_behavior?: string;
  readonly pending_update?: null;
};

/** Subscriptions as the Polar API sends them. */
function makeSdk(
  items: readonly object[],
  update: (id: string, body: SubscriptionUpdateBody) => Promise<object> = async (id) => ({ id }),
) {
  const iterList = async function* () {
    yield* items;
  };
  const updateSubscription = vi.fn(update);
  const sdk = {
    subscriptions: { iterList, update: updateSubscription },
  } as const;
  return { updateSubscription, sdk: asTestDouble<PolarSdk>()(sdk) };
}

const subscriptionLocked = () => Object.assign(new Error("SubscriptionLocked"), { statusCode: 409 });

const hosted = {
  mode: "hosted" as const,
  accessToken: Redacted.make("polar-token"),
  webhookSecret: Redacted.make("polar-webhook-secret"),
  server: "sandbox" as const,
  productId: "00000000-0000-4000-8000-000000000002",
};

describe("Polar provider boundary", () => {
  it("pins the Polar API version on every client", async () => {
    const fetch = vi.fn(async (_url: string, _init: RequestInit) =>
      Response.json({}),
    );
    vi.stubGlobal("fetch", fetch);
    try {
      await makePolarClient(hosted).customerSessions.create({
        external_customer_id: "user-1",
      });
    } finally {
      vi.unstubAllGlobals();
    }
    const [, sent] = fetch.mock.calls[0] ?? [];
    const [, built] = makePolarCore(hosted).buildRequest("GET", "/v1/");

    for (const init of [sent, built]) {
      expect(new Headers(init?.headers).get("Polar-Version")).toBe(
        POLAR_API_VERSION,
      );
    }
  });

  it("decodes active subscriptions into the billing protocol", async () => {
    const { sdk } = makeSdk([
      {
        id: "sub-pro",
        product_id: hosted.productId,
        current_period_end: "2026-04-01T00:00:00Z",
        cancel_at_period_end: true,
        units: 5,
        pending_update: { units: 3 },
      },
    ]);

    const provider = makePolarService(hosted, sdk);
    if (provider.mode !== "hosted") throw new Error("Expected hosted Polar");

    await expect(
      Effect.runPromise(provider.listActiveSubscriptions("org-1")),
    ).resolves.toEqual([
      {
        id: "sub-pro",
        productId: hosted.productId,
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
        cancelAtPeriodEnd: true,
        units: 5,
        pendingUpdate: { units: 3 },
      },
    ]);
  });

  it("clears a pending update and retries once when Polar locks the subscription", async () => {
    let locked = true;
    const { sdk, updateSubscription } = makeSdk([], async (id, body) => {
      if ("pending_update" in body) {
        locked = false;
        return { id };
      }
      if (locked) throw subscriptionLocked();
      return { id };
    });
    const provider = makePolarService(hosted, sdk);
    if (provider.mode !== "hosted") throw new Error("Expected hosted Polar");

    await Effect.runPromise(
      provider.setSubscriptionUnits({ subscriptionId: "sub-pro", units: 6, prorationBehavior: "invoice" }),
    );

    expect(updateSubscription.mock.calls).toEqual([
      ["sub-pro", { units: 6, proration_behavior: "invoice" }],
      ["sub-pro", { pending_update: null }],
      ["sub-pro", { units: 6, proration_behavior: "invoice" }],
    ]);
  });

  it("fails when Polar still locks the subscription after the pending update was cleared", async () => {
    const { sdk, updateSubscription } = makeSdk([], async (id, body) => {
      if ("pending_update" in body) return { id };
      throw subscriptionLocked();
    });
    const provider = makePolarService(hosted, sdk);
    if (provider.mode !== "hosted") throw new Error("Expected hosted Polar");

    const failure = await Effect.runPromise(
      Effect.flip(provider.setSubscriptionUnits({ subscriptionId: "sub-pro", units: 6, prorationBehavior: "invoice" })),
    );

    expect(failure).toBeInstanceOf(PolarFailure);
    expect(updateSubscription).toHaveBeenCalledTimes(3);
  });

  it("does not clear anything for a failure other than the lock", async () => {
    const { sdk, updateSubscription } = makeSdk([], async () => {
      throw Object.assign(new Error("Polar is down"), { statusCode: 500 });
    });
    const provider = makePolarService(hosted, sdk);
    if (provider.mode !== "hosted") throw new Error("Expected hosted Polar");

    const failure = await Effect.runPromise(
      Effect.flip(provider.setSubscriptionUnits({ subscriptionId: "sub-pro", units: 6, prorationBehavior: "invoice" })),
    );

    expect(failure).toMatchObject({ retriable: true });
    expect(updateSubscription).toHaveBeenCalledOnce();
  });

  it("classifies invalid provider payloads without exposing their body", async () => {
    const { sdk } = makeSdk([
      {
        id: "sub-invalid",
        product_id: hosted.productId,
        current_period_end: "not-a-date",
        cancel_at_period_end: false,
        units: 2,
        pending_update: null,
      },
    ]);

    const provider = makePolarService(hosted, sdk);
    if (provider.mode !== "hosted") throw new Error("Expected hosted Polar");
    const failure = await Effect.runPromise(
      Effect.flip(provider.listActiveSubscriptions("org-1")),
    );

    expect(failure).toBeInstanceOf(PolarFailure);
    expect(failure).toMatchObject({
      code: "invalid_response",
      retriable: false,
    });
    expect(failure.message).toBe("Polar list active subscriptions failed.");
    expect(failure.message).not.toContain("sub-invalid");
  });
});
