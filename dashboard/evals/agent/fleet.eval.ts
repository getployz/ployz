import { describe, it } from "@effect/vitest";
import { fakeText } from "@tanstack/ai/testing";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { serviceOperations } from "#/modules/service-act/service-act.server";
import { storeTestCloud } from "#/test/store-cloud";
import { ENVIRONMENT_IDS, evalFixture, ORGANIZATION } from "./fixture";
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

  it.live("leaves a Container whose stop went unanswered stopped, and reads its restart as unknown", () => Effect.gen(function* () {
    const { conversation, fleet, provided } = yield* evalFixture({
      faults: [{ fault: "drop_reply", machine: "web-1", command: "stop_container", nth: 1 }],
    });
    const model = fakeText();
    model.setResponses([{ toolCalls: [{ name: "service_restart", input: { service: "web", wait_seconds: 0 } }] }, { text: "Restarting." }]);
    yield* conversation(model, "fleet-restart").say("restart web");

    const asked = (yield* fleet?.ledger ?? Effect.succeed([]))
      .filter(({ command }) => command === "stop_container" || command === "start_container")
      .map(({ machine, command, container }) => `${machine} ${command} ${container}`);
    expect(asked.sort()).toEqual([
      "web-1 stop_container app-staging-web-1",
      "web-2 start_container app-staging-web-2",
      "web-2 start_container app-staging-web-5",
      "web-2 stop_container app-staging-web-2",
    ]);
    const [restart] = yield* provided(serviceOperations(ORGANIZATION, { environment: ENVIRONMENT_IDS.staging, service: "web" }, null, 1));
    expect(restart).toMatchObject({ run: "restart web", state: "unknown" });
    expect(restart?.servers.map(({ server, stopped, started }) => ({ server, stopped, started })).sort((a, b) => a.server.localeCompare(b.server))).toEqual([
      { server: "web-1", stopped: "unknown", started: null },
      { server: "web-2", stopped: "yes", started: "yes" },
      { server: "web-2", stopped: "yes", started: "yes" },
    ]);
  }));
});
