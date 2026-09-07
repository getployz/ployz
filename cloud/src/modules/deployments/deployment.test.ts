import { Option, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { DeploymentTriggerOrigin } from "./deployment";

describe("DeploymentTriggerOrigin", () => {
  it("accepts the durable manual, Git, and first-connect identities", () => {
    const decode = Schema.decodeUnknownOption(DeploymentTriggerOrigin);

    expect(
      Option.isSome(decode({ origin: "manual", actorId: "actor-1" })),
    ).toBe(true);
    expect(
      Option.isSome(
        decode({
          origin: "github",
          deliveryId: "delivery-1",
          branchEvaluationRevision: 1,
          installationId: 17,
          repositoryId: 42,
        }),
      ),
    ).toBe(true);
    expect(
      Option.isSome(
        decode({
          origin: "first_connect",
          machineId: "0123456789abcdef0123456789abcdef",
        }),
      ),
    ).toBe(true);
  });

  it("rejects invalid identities and non-finite Git numbers", () => {
    const decode = Schema.decodeUnknownOption(DeploymentTriggerOrigin);

    expect(Option.isNone(decode({ origin: "manual", actorId: "" }))).toBe(
      true,
    );
    expect(
      Option.isNone(
        decode({
          origin: "github",
          deliveryId: "delivery-1",
          branchEvaluationRevision: Number.POSITIVE_INFINITY,
          installationId: 17,
          repositoryId: 42,
        }),
      ),
    ).toBe(true);
    expect(
      Option.isNone(
        decode({ origin: "first_connect", machineId: "machine-1" }),
      ),
    ).toBe(true);
  });
});
