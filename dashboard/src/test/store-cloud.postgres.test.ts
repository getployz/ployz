import { it } from "@effect/vitest";
import { Context, Effect, Layer, Option } from "effect";
import { expect, expectTypeOf } from "vitest";
import { type StoreCloud, storeTestCloud, storeTestCloudWith } from "#/test/store-cloud";

class Extra extends Context.Service<Extra, { readonly n: number }>()("test/Extra") {}

const providesExtra = Layer.succeed(Extra, { n: 1 });
const failsWithWords = Layer.effect(Extra, Effect.fail("down"));

interface CloudEnv {
  readonly env: Readonly<Record<string, string>>;
}

function forwarded<T extends CloudEnv>(options: T) {
  return storeTestCloud(options);
}

it.live("builds what the layer adds, and fails as it fails", () => Effect.gen(function* () {
  const services = yield* storeTestCloudWith(providesExtra, { env: {} });
  expectTypeOf(services).toEqualTypeOf<Context.Context<StoreCloud | Extra>>();
  expect(Context.get(services, Extra)).toEqual({ n: 1 });

  const failing = storeTestCloudWith(failsWithWords);
  expectTypeOf<string>().toExtend<Effect.Error<typeof failing>>();
  expect(yield* Effect.flip(failing)).toBe("down");

  // @ts-expect-error extra services go through storeTestCloudWith
  storeTestCloud({ env: {}, with: providesExtra });
}));

it.live("builds only Cloud when a generic wrapper passes options that carry `with`", () => Effect.gen(function* () {
  const built = forwarded({ env: {}, with: failsWithWords });
  expectTypeOf<Effect.Success<typeof built>>().toEqualTypeOf<Context.Context<StoreCloud>>();
  expectTypeOf<string>().not.toExtend<Effect.Error<typeof built>>();
  const services = yield* built;
  expect(Option.isNone(Context.getOption(services, Extra)), "Extra was built").toBe(true);
}));

it.live("builds only Cloud when options typed without `with` carry one", () => Effect.gen(function* () {
  const widened = { env: {}, with: providesExtra } as CloudEnv;
  const services = yield* storeTestCloud(widened);
  expectTypeOf(services).toEqualTypeOf<Context.Context<StoreCloud>>();
  expect(Option.isNone(Context.getOption(services, Extra)), "Extra was built").toBe(true);
}));
