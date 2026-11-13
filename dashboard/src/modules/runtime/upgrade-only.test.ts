import { assert, it } from "@effect/vitest";
import { Effect, Exit } from "effect";
import { requirements, ServerLacks } from "#/modules/runtime/daemon-compat";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { runtimeWatchFrameFixture, runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { REACHES, upgradeOnlyOnOlder } from "#/modules/runtime/upgrade-only";

const OLD = "machine-old";
const NEW = "machine-new";

const machine = (id: string, capabilities: readonly string[]) => {
  const fixture = runtimeWatchMachineFixture(id, id);
  return { machine: { ...fixture, runtime: { ...fixture.runtime, capabilities: [...capabilities] } }, membership: "active", storage: null };
};

const frame = runtimeWatchFrameFixture({
  machines: [machine(OLD, ["ployz.logs.tail.v1"]), machine(NEW, [requirements.enforced.capability])] as never,
});

/** A session that records each call it is sent. */
function recording() {
  const sent: string[] = [];
  const session = Object.fromEntries(Object.keys(REACHES).map((name) => [name, (..._args: unknown[]) => {
    sent.push(name);
    return name === "watchFirstFrame" ? Effect.succeed(frame) : Effect.succeed(undefined);
  }]));
  return { sent, session: session as unknown as PloyzSession };
}

type Call = (...args: unknown[]) => unknown;

/** Each call with every Server it names being `target`; a Volume verb is a seed unless `inspect`. */
function argsAimedAt(name: string, target: string, inspect = false): unknown[] {
  if (name === "volumeSwitch") return [target, { command: inspect ? "inspect_volume_copy" : "seed_volume", payload: {} }];
  return [target, "container", target];
}

const ALLOWED_ON_OLDER = [
  "logs", "logHistory", "metricHistory", "inspect", "inspectMachine", "cluster", "observeEnrollment",
  "dataLossIfMachineRemoved", "dataLossIfNamespaceDestroyed", "inspectContainer", "agentAsk", "observeService",
  "requestMachineUpgrade", "setManagementClient", "clearManagementClient", "setPushTarget", "publishCertificateMaterial",
];

it.effect("sends a Server on Ployz 0.3 nothing but upgrades, reads and Cloud's own connection upkeep", () =>
  Effect.gen(function* () {
    const { sent, session } = recording();
    const gated = upgradeOnlyOnOlder(session);
    yield* Effect.forEach(Object.keys(REACHES).filter((name) => name !== "watch" && name !== "watchFirstFrame"), (name) => {
      const called = (gated[name as keyof PloyzSession] as Call)(...argsAimedAt(name, OLD));
      return Effect.isEffect(called) ? Effect.exit(called as Effect.Effect<unknown, unknown>) : Effect.void;
    });
    yield* (gated.volumeSwitch as Call)(...argsAimedAt("volumeSwitch", OLD, true)) as Effect.Effect<unknown>;
    assert.deepStrictEqual(sent.filter((name) => name !== "watchFirstFrame").sort(), [...ALLOWED_ON_OLDER, "volumeSwitch"].sort());
  }));

it.effect("refuses a change to a Server on Ployz 0.3 naming it, and sends the same change to a Server on 0.4", () =>
  Effect.gen(function* () {
    const { sent, session } = recording();
    const gated = upgradeOnlyOnOlder(session);
    const refused = yield* Effect.exit(gated.startContainer(OLD, "container", null));
    assert.deepStrictEqual(refused, Exit.fail(new ServerLacks({ machineId: OLD, capability: requirements.enforced.capability })));
    yield* gated.startContainer(NEW, "container", null);
    assert.deepStrictEqual(sent, ["watchFirstFrame", "startContainer"]);
  }));

it.effect("refuses a change that reaches every Server while one of them runs Ployz 0.3", () =>
  Effect.gen(function* () {
    const { sent, session } = recording();
    const refused = yield* Effect.exit(upgradeOnlyOnOlder(session).destroyNamespace("shop-prod", { volumes: [] } as never, null));
    assert.isTrue(Exit.isFailure(refused));
    assert.notInclude(sent, "destroyNamespace");
  }));
