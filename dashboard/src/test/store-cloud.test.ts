import { Context, Effect, Layer } from "effect";
import { describe, expectTypeOf, it } from "vitest";
import { type StoreCloud, storeTestCloud } from "#/test/store-cloud";

class Extra extends Context.Service<Extra, { readonly n: number }>()("test/Extra") {}

const failsWithWords = Layer.effect(Extra, Effect.fail("down"));

interface MaybeExtra {
  readonly env: Readonly<Record<string, string>>;
  readonly with?: typeof failsWithWords;
}

describe("storeTestCloud", () => {
  // Catches options held in a variable picking the plain overload, which drops what `with` adds and how it fails.
  it("types what `with` adds and how it fails, however the options arrive", () => {
    const options = { env: {}, with: failsWithWords };
    const built = storeTestCloud(options);
    expectTypeOf<Effect.Success<typeof built>>().toEqualTypeOf<Context.Context<StoreCloud | Extra>>();
    expectTypeOf<string>().toExtend<Effect.Error<typeof built>>();

    const plain = { env: {} };
    const plainBuilt = storeTestCloud(plain);
    expectTypeOf<Effect.Success<typeof plainBuilt>>().toEqualTypeOf<Context.Context<StoreCloud>>();

    const maybe: MaybeExtra = { env: {} };
    // @ts-expect-error a `with` that may be absent can't promise its services
    storeTestCloud(maybe);
  });
});
