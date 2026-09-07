import { describe, expect, it } from "vitest";
import { resolvePolarConfiguration } from "#/config/polar";

const complete = {
  accessToken: "polar-token",
  webhookSecret: "polar-webhook",
  freeProductId: "free-product",
  soloProductId: "solo-product",
  teamsProductId: "teams-product",
};

describe("resolvePolarConfiguration", () => {
  it("treats a wholly absent Polar environment as self-hosted", () => {
    expect(resolvePolarConfiguration({})).toEqual({ mode: "self_hosted" });
  });

  it("accepts a complete hosted configuration", () => {
    expect(resolvePolarConfiguration(complete)).toEqual({
      mode: "hosted",
      accessToken: "polar-token",
      webhookSecret: "polar-webhook",
      productIds: {
        free: "free-product",
        solo: "solo-product",
        teams: "teams-product",
      },
    });
  });

  it("rejects hosted billing without webhook handling", () => {
    const { webhookSecret: _, ...withoutWebhook } = complete;

    expect(() => resolvePolarConfiguration(withoutWebhook)).toThrow(
      /webhook secret/,
    );
  });

  it("accepts matching transitional product aliases", () => {
    expect(
      resolvePolarConfiguration({
        ...complete,
        hobbyProductId: complete.soloProductId,
        proProductId: complete.teamsProductId,
      }),
    ).toMatchObject({ mode: "hosted" });
  });

  it("rejects partial, conflicting, or duplicate hosted configuration", () => {
    expect(() =>
      resolvePolarConfiguration({ accessToken: "polar-token" }),
    ).toThrow(/webhook secret/);
    expect(() =>
      resolvePolarConfiguration({
        ...complete,
        hobbyProductId: "other-solo-product",
      }),
    ).toThrow(/conflicts/);
    expect(() =>
      resolvePolarConfiguration({
        ...complete,
        teamsProductId: complete.soloProductId,
      }),
    ).toThrow(/distinct/);
  });
});
