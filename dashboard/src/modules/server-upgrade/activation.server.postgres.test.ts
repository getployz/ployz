import type { MachineDetails, MachineId, RecipientKeySet, SetRecipientKeyRequest, Signed } from "@ployz/sdk";
import { Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSession } from "#/modules/runtime/ployz.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { ENFORCED_CAPABILITY, type PasskeyAnswered } from "./activation";
import { activateServers, generateRecipient, installRecipient, readActivation } from "./activation.server";

const ORG = "00000000-0000-4000-8000-0000000ac701";
const ADA = "00000000-0000-4000-8000-0000000ac702";
const SLUG = "acme";
const actor = { userId: ADA };
// SAFETY: 32 hex characters, the form a MachineId parses.
const WEB_1 = "1".repeat(32) as MachineId;
// SAFETY: as above.
const WEB_2 = "2".repeat(32) as MachineId;
const OPERATION = "a".repeat(32);
const IDENTITY = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const RECIPIENT = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
const ASSERTION = { credential_id: "AQID", authenticator_data: "AQID", client_data_json: "AQID", signature: "AQID" };
const member = (id: string, fill: number) => ({ id, public_key: Array.from({ length: 32 }, () => fill), subnet: `10.210.${fill}.0/24` });
/** A starting set's or rotation's body, as far as Cloud reads it. */
type Body = {
  readonly operation: string;
  readonly members?: readonly ReturnType<typeof member>[];
  readonly cluster?: string;
  readonly recipient?: string;
  readonly signer?: string;
};
const answered = (body: Body): PasskeyAnswered => ({ body: JSON.stringify(body), signatures: [], assertions: [ASSERTION] });
const STARTING_SET = answered({ operation: OPERATION, members: [member(WEB_2, 2), member(WEB_1, 1)], signer: "passkey" });
const ROTATION = answered({ cluster: IDENTITY, operation: OPERATION, recipient: RECIPIENT, members: [member(WEB_1, 1), member(WEB_2, 2)], signer: "passkey" });

let harness: PostgresTestHarness;

/** Each Server as it answers: awaiting, restarting for `looks` more Inspects, or activated under an operation. */
type Phase = { readonly kind: "awaiting" } | { kind: "restarting"; looks: number; readonly operation: string } | { readonly kind: "activated"; readonly operation: string };
let phases: Map<string, Phase>;
let recipients: Map<string, string>;
let refusing: Set<string>;
let log: string[];
let carried: Signed[];
let inspects = 0;

const unavailable = (operation: string, message: string) => new PloyzProviderError({ operation, cause: { code: "unavailable", message } });

function details(id: string): MachineDetails {
  const phase = phases.get(id) ?? { kind: "awaiting" };
  const fill = id === WEB_1 ? 1 : 2;
  return asTestDouble<MachineDetails>()({
    machine: {
      id,
      public_key: member(id, fill).public_key,
      subnet: member(id, fill).subnet,
      runtime: { capabilities: [ENFORCED_CAPABILITY] },
    },
    sealing: { kind: "held", public_key: IDENTITY },
    sealing_recipient: recipients.get(id) ?? null,
    activation: {
      state: phase.kind === "restarting" ? "activating" : phase.kind,
      operation: phase.kind === "awaiting" ? undefined : phase.operation,
      legacy_members: [],
      legacy_keys: [],
      private_access: id === WEB_1 ? { tailnet: "acme.ts.net" } : null,
    },
  });
}

const session: ScopedRuntimeClientSession = {
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    watchFirstFrame: () => Effect.sync(() => runtimeWatchFrameFixture({ machines: [
      runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(WEB_1, "web-1") }),
      runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(WEB_2, "web-2") }),
    ] })),
    inspectMachine: (id: string) => Effect.suspend(() => {
      inspects += 1;
      const phase = phases.get(id);
      if (phase?.kind === "restarting") {
        if (phase.looks > 0) {
          phase.looks -= 1;
          return Effect.fail(unavailable("inspect machine", "restarting"));
        }
        phases.set(id, { kind: "activated", operation: phase.operation });
      }
      return Effect.succeed(details(id));
    }),
    activateEnforcement: (id: string, startingSet: Signed) => Effect.suspend(() => {
      log.push(`activate ${id}`);
      carried.push(startingSet);
      if (refusing.has(id)) {
        return Effect.fail(new PloyzProviderError({ operation: "activate enforcement", cause: { code: "forbidden", message: "This server lists a server you added while enforcement was off." } }));
      }
      // The daemon restarts, so Inspect fails a while before it reports the set committed.
      phases.set(id, { kind: "restarting", looks: 2, operation: OPERATION });
      return Effect.succeed(asTestDouble<never>()({}));
    }),
    setRecipientKey: (id: string, request: SetRecipientKeyRequest) => Effect.suspend((): Effect.Effect<RecipientKeySet, PloyzProviderError> => {
      log.push(`${request.step} ${id}`);
      if (refusing.has(id)) return Effect.fail(new PloyzProviderError({ operation: "set recipient key", cause: { code: "forbidden", message: "This server refused the key." } }));
      switch (request.step) {
        case "generate":
          return Effect.succeed({ step: "generated", recipient: RECIPIENT });
        case "install":
          recipients.set(id, RECIPIENT);
          return Effect.succeed({ step: "installed", recipient: RECIPIENT });
        case "share":
          return Effect.succeed({ step: "shared", to: request.to, sealed: "c2VhbGVk" });
      }
    }),
  }),
};

const runtime = asTestDouble<OrganizationRuntime["Service"]>()({
  cancel: () => Effect.void,
  open: () => Effect.succeed(session),
});

const run = <A, E>(effect: Effect.Effect<A, E, OrganizationRuntime | Database | SecretEncryption>) =>
  harness.runEffect(effect.pipe(Effect.provideService(OrganizationRuntime, runtime)));

/**
 * `effect` with its waits on a test clock: each time a Server is asked again while it restarts, the clock moves a
 * second, so the next look comes without any real wait.
 */
const onTestClock = <A, E, R>(effect: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  const fiber = yield* Effect.forkChild(effect);
  let seen = 0;
  while (fiber.pollUnsafe() === undefined) {
    yield* Effect.promise(() => vi.waitFor(() => {
      if (fiber.pollUnsafe() === undefined && inspects <= seen) throw new Error("no new look yet");
    }));
    seen = inspects;
    yield* TestClock.adjust("1 second");
  }
  return yield* Fiber.join(fiber);
}).pipe(Effect.provide(TestClock.layer()));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

beforeEach(async () => {
  phases = new Map();
  recipients = new Map();
  refusing = new Set();
  log = [];
  carried = [];
  inspects = 0;
  await harness.pool.query(`
    truncate table "user", organization cascade;
    insert into organization (id, name, slug) values ('${ORG}', 'Acme', '${SLUG}');
    insert into "user" (id, email, name) values ('${ADA}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values (gen_random_uuid(), '${ORG}', '${ADA}', 'owner', now());
  `);
});

describe("activateServers", () => {
  it("hands the set to each Server in ID order, waits through each restart, and reports the router each kept", async () => {
    const servers = await run(onTestClock(activateServers(actor, SLUG, STARTING_SET)));

    expect(log).toEqual([`activate ${WEB_1}`, `activate ${WEB_2}`]);
    expect(carried).toEqual([
      { body: STARTING_SET.body, signatures: [], assertions: [ASSERTION] },
      { body: STARTING_SET.body, signatures: [], assertions: [ASSERTION] },
    ]);
    expect(servers).toEqual([
      { id: WEB_1, name: "web-1", kind: "activated", privateAccess: { tailnet: "acme.ts.net" } },
      { id: WEB_2, name: "web-2", kind: "activated", privateAccess: null },
    ]);
  });

  it("names a Server that refused in its own words and still activates the rest", async () => {
    refusing.add(WEB_1);
    const servers = await run(onTestClock(activateServers(actor, SLUG, STARTING_SET)));

    expect(servers).toEqual([
      { id: WEB_1, name: "web-1", kind: "refused", why: "This server lists a server you added while enforcement was off." },
      { id: WEB_2, name: "web-2", kind: "activated", privateAccess: null },
    ]);
  });

  it("refuses a body that isn't a starting set before any Server hears of it", async () => {
    await expect(run(activateServers(actor, SLUG, answered({ operation: "nope" })))).rejects.toMatchObject({ _tag: "Validation" });
    expect(log).toEqual([]);
  });

  it("refuses someone who can't operate the Servers before any Server hears of it", async () => {
    await harness.pool.query(`update member set role = 'member' where user_id = '${ADA}'`);
    await expect(run(activateServers(actor, SLUG, STARTING_SET))).rejects.toMatchObject({ _tag: "Forbidden" });
    expect(log).toEqual([]);
  });
});

describe("generateRecipient", () => {
  it("has the lowest-ID Server that reports the activation committed generate the key", async () => {
    phases.set(WEB_2, { kind: "activated", operation: OPERATION });
    phases.set(WEB_1, { kind: "activated", operation: OPERATION });

    expect(await run(generateRecipient(actor, SLUG, OPERATION))).toEqual({ generator: WEB_1, recipient: RECIPIENT });
    expect(log).toEqual([`generate ${WEB_1}`]);
  });

  it("refuses when no Server reports that activation, without asking any to generate", async () => {
    phases.set(WEB_1, { kind: "activated", operation: "b".repeat(32) });
    await expect(run(generateRecipient(actor, SLUG, OPERATION))).rejects.toMatchObject({ _tag: "Validation" });
    expect(log).toEqual([]);
  });
});

describe("installRecipient", () => {
  it("installs on the generator from its own copy, then shares to and installs on every other Member", async () => {
    const installs = await run(installRecipient(actor, { organizationSlug: SLUG, rotation: ROTATION, generator: WEB_2 }));

    expect(log).toEqual([`install ${WEB_2}`, `share ${WEB_2}`, `install ${WEB_1}`]);
    expect(installs).toEqual([
      { id: WEB_2, name: "web-2", kind: "installed" },
      { id: WEB_1, name: "web-1", kind: "installed" },
    ]);
  });

  it("shares nothing when the generator refuses the key", async () => {
    refusing.add(WEB_1);
    const installs = await run(installRecipient(actor, { organizationSlug: SLUG, rotation: ROTATION, generator: WEB_1 }));

    expect(log).toEqual([`install ${WEB_1}`]);
    expect(installs).toEqual([
      { id: WEB_1, name: "web-1", kind: "refused", why: "This server refused the key." },
      { id: WEB_2, name: "web-2", kind: "refused", why: "The server that made the key didn't install it, so it wasn't shared." },
    ]);
  });

  it("refuses a generator the rotation doesn't name", async () => {
    const rotation = answered({ cluster: IDENTITY, operation: OPERATION, recipient: RECIPIENT, members: [member(WEB_1, 1)], signer: "passkey" });
    await expect(run(installRecipient(actor, { organizationSlug: SLUG, rotation, generator: WEB_2 }))).rejects.toMatchObject({ _tag: "Validation" });
    expect(log).toEqual([]);
  });
});

describe("readActivation", () => {
  it("reads each Server's own report", async () => {
    phases.set(WEB_1, { kind: "activated", operation: OPERATION });
    recipients.set(WEB_1, RECIPIENT);
    const servers = await run(readActivation(ORG));

    expect(servers.map(({ name, activation, recipient }) => ({ name, state: activation?.state, recipient }))).toEqual([
      { name: "web-1", state: "activated", recipient: RECIPIENT },
      { name: "web-2", state: "awaiting", recipient: null },
    ]);
  });
});

