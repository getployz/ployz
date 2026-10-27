import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect } from "vitest";
import { retireOldCopies } from "#/modules/config-store/retire-old.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000af02";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000af03";

it.live("a cleanup of a Deployment the Store no longer has, gone with its Environment, retires nothing and ends", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const requested = { organizationId: ORGANIZATION, environmentId: "pr-1", deploymentId: DEPLOYMENT, number: 2 };
    expect(yield* provided(retireOldCopies(requested))).toEqual({ left: [], kept: [] });
  }), 60_000);
