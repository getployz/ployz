import { beforeEach, describe, expect, it } from "vitest";
import type { CopyObservation, MachineId } from "@ployz/sdk";
import { Effect, Exit } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { asSdkFailure, PloyzProviderError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { asRemoveMachineOutcome, observeLeavingServerActivity } from "./machine-removal.server";

const identities = [
  {
    kind: "docker_volume" as const,
    id: {
      machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
      name: "data",
    },
  },
];

describe("asRemoveMachineOutcome", () => {
  it("maps rust missing identities onto the Data Loss reopen path", async () => {
    await expect(
      Effect.runPromise(
        asRemoveMachineOutcome(
          Effect.fail(new MissingDataLossIdentities(identities)),
        ),
      ),
    ).resolves.toEqual({ kind: "missing_identities", identities });
  });

  it("ends on the Server's refusal instead of retrying it", async () => {
    const message = "Server two answers and holds copies of Volumes vol-1; removing it without a reset leaves them behind. No changes made.";
    await expect(
      Effect.runPromise(
        asRemoveMachineOutcome(
          Effect.fail(new PloyzProviderError({ operation: "remove machine membership", cause: { code: "conflict", message } })),
        ),
      ),
    ).resolves.toEqual({ kind: "refused", failureCode: "conflict", message });
  });

  it("ends on a daemon that lacks the step, naming the Server", async () => {
    await expect(
      Effect.runPromise(
        asRemoveMachineOutcome(
          Effect.fail(new ServerLacks({ machineId: "machine-1", capability: "ployz.machine.forget.v1" })),
          [{ id: "machine-1", name: "fsn-1" }],
        ),
      ),
    ).resolves.toMatchObject({ kind: "refused", failureCode: "server_lacks" });
  });

  it("keeps a Leave the Server answered before its Forget failed, as a result the step remembers", async () => {
    const cause = {
      code: "unavailable",
      message: "the Cluster store is unavailable",
      details: { left: true, reset_warning: "Docker reset failed" },
    };
    await expect(
      Effect.runPromise(asRemoveMachineOutcome(Effect.fail(asSdkFailure("remove machine", cause)))),
    ).resolves.toEqual({ kind: "left", resetWarning: "Docker reset failed" });
  });

  it("leaves any other failure to the step's retries", async () => {
    const exit = await Effect.runPromiseExit(
      asRemoveMachineOutcome(
        Effect.fail(new PloyzProviderError({ operation: "remove machine membership", cause: { code: "unavailable", message: "transport error" } })),
      ),
    );
    expect(exit._tag).toBe("Failure");
  });
});

describe("observeLeavingServerActivity", () => {
  const leaving = "leaving" as MachineId;
  const other = "other" as MachineId;
  const writerOn = (machine: MachineId, name: string) =>
    asTestDouble<CopyObservation["copies"][number]>()({ machine_id: machine, name, role: "writer" });
  let frame: "answers" | "omits" | "times out";
  let membership: string;
  let observed: CopyObservation | null;
  let waited: number | null;

  const observe = (lastTry = false) => {
    const session = asTestDouble<PloyzSession>()({
      watchFirstFrame: (timeoutMs: number) => {
        waited = timeoutMs;
        return frame === "times out"
          ? Effect.fail(new RuntimeConnectionFailure({ cause: new Error("Runtime watch timed out") }))
          : Effect.succeed({
            machines: [
              { machine: { id: other, name: "fsn-1" }, membership: "up" },
              ...frame === "answers" ? [{ machine: { id: leaving, name: "fsn-2" }, membership }] : [],
            ],
            containers: [],
          });
      },
    });
    const observeCopies = () => observed === null
      ? Effect.fail(new PloyzProviderError({ operation: "observe copies", cause: new Error("transport error") }))
      : Effect.succeed(observed);
    return Effect.runPromiseExit(observeLeavingServerActivity({ organizationId: "org-1", machineId: leaving, lastTry }, observeCopies).pipe(
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntime["Service"]>()({
        open: () => Effect.succeed({ status: "connected" as const, connected: session }),
      })),
      Effect.provideService(Database, asTestDouble<Database["Service"]>()({})),
      Effect.provideService(SecretEncryption, asTestDouble<SecretEncryption["Service"]>()({})),
    ));
  };

  beforeEach(() => {
    frame = "answers";
    membership = "up";
    waited = null;
    observed = { copies: [writerOn(leaving, "data"), writerOn(other, "logs")], unanswered: [] };
  });

  it("names the leaving Server and the Volumes it writes", async () => {
    expect(await observe()).toEqual(Exit.succeed({ machine: { id: leaving, name: "fsn-2" }, writes: ["data"] }));
  });

  it("waits for the Cluster frame as long as the removal itself does", async () => {
    await observe();
    expect(waited).toBe(RUNTIME_FRAME_TIMEOUT_MS);
  });

  it("fails, for the step to retry, when the Cluster frame times out", async () => {
    frame = "times out";
    expect(await observe()).toMatchObject({ _tag: "Failure" });
  });

  it("fails, for the step to retry, when the copies cannot be observed", async () => {
    observed = null;
    expect(await observe()).toMatchObject({ _tag: "Failure" });
  });

  it("names no Server when the Cluster answers without the leaving Machine", async () => {
    frame = "omits";
    expect(await observe()).toEqual(Exit.succeed({ machine: { id: leaving, name: null }, writes: ["data"] }));
  });

  it("fails, for the step to retry, when the leaving Machine reads up but its copies do not answer", async () => {
    observed = { copies: [writerOn(other, "logs")], unanswered: [leaving] };
    expect(await observe()).toMatchObject({ _tag: "Failure" });
  });

  it("writes nothing on the last try when the leaving Machine reads up but its copies still do not answer", async () => {
    observed = { copies: [writerOn(other, "logs")], unanswered: [leaving] };
    expect(await observe(true)).toEqual(Exit.succeed({ machine: { id: leaving, name: "fsn-2" }, writes: [] }));
  });

  it("still fails on the last try when the Cluster frame times out", async () => {
    frame = "times out";
    expect(await observe(true)).toMatchObject({ _tag: "Failure" });
  });

  it("writes nothing when the leaving Machine is down and its copies do not answer", async () => {
    membership = "down";
    observed = { copies: [writerOn(other, "logs")], unanswered: [leaving] };
    expect(await observe()).toEqual(Exit.succeed({ machine: { id: leaving, name: "fsn-2" }, writes: [] }));
  });

  it("reads past another Server that does not answer", async () => {
    observed = { copies: [writerOn(leaving, "data")], unanswered: [other] };
    expect(await observe()).toEqual(Exit.succeed({ machine: { id: leaving, name: "fsn-2" }, writes: ["data"] }));
  });
});
