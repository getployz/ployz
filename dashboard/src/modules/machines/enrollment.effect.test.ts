import { assert, it } from "@effect/vitest";
import { Effect, Exit, Schema } from "effect";
import { MintMachineEnrollmentInput } from "#/modules/machines/enrollment";

it.effect("rejects extra authenticated enrollment fields", () =>
  Effect.gen(function* () {
    const exit = yield* Schema.decodeUnknownEffect(MintMachineEnrollmentInput)(
      { organizationSlug: "acme", userId: "forged" },
      { onExcessProperty: "error" },
    ).pipe(Effect.exit);

    assert.isTrue(Exit.isFailure(exit));
  }),
);
