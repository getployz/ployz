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

/** Subscriptions as the Polar API sends them. */
function makeSdk(items: readonly object[]) {
  const iterList = vi.fn(async function* () {
    yield* items;
  });
  const sdk = {
    subscriptions: { iterList },
    checkouts: { create: vi.fn() },
  } as const;
  return { iterList, sdk: asTestDouble<PolarSdk>()(sdk) };
}

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

  it("does not construct a Polar client in self-hosted mode", () => {
    const sdk = makeSdk([]);

    expect(makePolarService({ mode: "self_hosted" }, sdk.sdk)).toEqual({
      mode: "self_hosted",
    });
    expect(sdk.iterList).not.toHaveBeenCalled();
  });

  it("decodes active subscriptions into the billing protocol", async () => {
    const { sdk } = makeSdk([
      {
        id: "sub-pro",
        product_id: hosted.productId,
        current_period_end: "2026-04-01T00:00:00Z",
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
      },
    ]);
  });

  it("classifies invalid provider payloads without exposing their body", async () => {
    const { sdk } = makeSdk([
      {
        id: "sub-invalid",
        product_id: hosted.productId,
        current_period_end: "not-a-date",
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
