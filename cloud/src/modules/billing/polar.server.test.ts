import { describe, expect, it, vi } from "vitest";
import { Effect, Redacted } from "effect";
import type { Polar as PolarSdk } from "@polar-sh/sdk";
import type { PolarSubscription } from "#/modules/billing/billing";
import {
  makePolarService,
  PolarFailure,
} from "#/modules/billing/polar-provider.server";
import { asTestDouble } from "#/lib/test-double";

function makeSdk(items: readonly PolarSubscription[]) {
  const list = vi.fn(async () => ({
    [Symbol.asyncIterator]: async function* () {
      yield { result: { items } };
    },
  }));
  const sdk = {
    subscriptions: {
      list,
      create: vi.fn(),
      update: vi.fn(),
    },
    products: { get: vi.fn() },
    checkouts: { create: vi.fn() },
  } as const;
  return { list, sdk: asTestDouble<PolarSdk>()(sdk) };
}

const hosted = {
  mode: "hosted" as const,
  accessToken: Redacted.make("polar-token"),
  webhookSecret: Redacted.make("polar-webhook-secret"),
  server: "sandbox" as const,
  productIds: {
    free: "00000000-0000-4000-8000-000000000001",
    solo: "00000000-0000-4000-8000-000000000002",
    teams: "00000000-0000-4000-8000-000000000003",
  },
};

describe("Polar provider boundary", () => {
  it("does not construct a Polar client in self-hosted mode", () => {
    const sdk = makeSdk([]);

    expect(makePolarService({ mode: "self_hosted" }, sdk.sdk)).toEqual({
      mode: "self_hosted",
    });
    expect(sdk.list).not.toHaveBeenCalled();
  });

  it("decodes active subscriptions into the billing protocol", async () => {
    const { sdk } = makeSdk([
      {
        id: "sub-teams",
        productId: hosted.productIds.teams,
        amount: 2900,
        currency: "usd",
        currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
      },
    ]);

    const provider = makePolarService(hosted, sdk);
    if (provider.mode !== "hosted") throw new Error("Expected hosted Polar");

    await expect(
      Effect.runPromise(provider.listActiveSubscriptions("org-1")),
    ).resolves.toEqual([
      {
        id: "sub-teams",
        productId: hosted.productIds.teams,
        amount: 2900,
        currency: "usd",
        currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
      },
    ]);
  });

  it("classifies invalid provider payloads without exposing their body", async () => {
    const { sdk } = makeSdk([
      {
        id: "sub-invalid",
        productId: hosted.productIds.solo,
        amount: Number.POSITIVE_INFINITY,
        currency: "usd",
        currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
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
