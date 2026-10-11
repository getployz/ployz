import { describe, it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { storeTestCloud } from "#/test/store-cloud";
import { ORGANIZATION } from "./fixture";
import { startFleet } from "./fleet";

describe("the fake fleet", () => {
  it.live("answers Cloud's runtime session through the SDK and records what it was asked", () => Effect.gen(function* () {
    const fleet = yield* startFleet({});
    const services = yield* Layer.build(yield* storeTestCloud({ fleet }));
    const session = yield* OrganizationRuntime.use((runtime) => runtime.open(ORGANIZATION)).pipe(Effect.provide(services));
    expect(session.status).toBe("connected");
    if (session.status !== "connected") return;
    const events = yield* session.connected.logs({ filter: { serviceName: "web" }, tail: 50, follow: false });
    const messages = yield* Effect.promise(async () => {
      const said: string[] = [];
      for await (const event of events) if (event.type === "record") said.push(event.record.message);
      return said;
    });
    expect(messages).toContain("starting session store");
    const ledger = yield* fleet.ledger;
    expect(ledger.filter(({ command }) => command === "container_logs").map(({ machine }) => machine)).toContain("web-1");
    expect(ledger.filter(({ machine }) => machine === "web-3")).toEqual([]);
  }));
});
