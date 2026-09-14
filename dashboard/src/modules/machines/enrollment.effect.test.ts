import { assert, it } from "@effect/vitest";
import { Effect, Schema } from "effect";
import { MintMachineEnrollmentInput } from "#/modules/machines/enrollment";

it.effect("rejects extra authenticated enrollment fields", () =>
  Effect.gen(function* () {
    const failure = yield* Schema.decodeUnknownEffect(MintMachineEnrollmentInput)(
      { organizationSlug: "acme", userId: "forged" },
      { onExcessProperty: "error" },
    ).pipe(Effect.flip);

    assert.isTrue(Schema.isSchemaError(failure));
  }),
);
