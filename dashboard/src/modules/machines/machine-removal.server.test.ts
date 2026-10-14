import { describe, expect, it } from "vitest";
import type { MachineId, ObservedCopy } from "@ployz/sdk";
import { Effect } from "effect";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { asSdkFailure, PloyzProviderError } from "#/modules/runtime/ployz.server";
import { asRemoveMachineOutcome, heldOff } from "./machine-removal.server";

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

describe("heldOff", () => {
  const id = (server: string) => server.padEnd(32, "0") as MachineId;
  const servers = ["a", "b", "c"].map((server) => ({ id: id(server), name: `fsn-${server}` }));
  const answered = (copies: ObservedCopy[], unanswered: MachineId[] = []) => ({ copies, unanswered });

  it("names the source writer of a Move whose lane commits to the leaving Server after the observation", () => {
    const copies: ObservedCopy[] = [{ machine_id: id("a"), name: "data", role: "writer" }, { machine_id: id("b"), name: "data", role: "slot" }];
    expect(heldOff(answered(copies), servers, id("b"))).toEqual({ data: "fsn-a" });
  });

  it("names the source writer over a slot on a third Server listed before it", () => {
    const copies: ObservedCopy[] = [{ machine_id: id("c"), name: "data", role: "slot" }, { machine_id: id("a"), name: "data", role: "writer" }];
    expect(heldOff(answered(copies), servers, id("b"))).toEqual({ data: "fsn-a" });
  });

  it("names the source writer even when a third Server did not answer", () => {
    expect(heldOff(answered([{ machine_id: id("a"), name: "data", role: "writer" }], [id("c")]), servers, id("b"))).toEqual({ data: "fsn-a" });
  });

  it("names no copy when a Move committed before the observation left two slots and no writer", () => {
    const copies: ObservedCopy[] = [{ machine_id: id("c"), name: "data", role: "slot" }, { machine_id: id("a"), name: "data", role: "slot" }];
    expect(heldOff(answered(copies), servers, id("b"))).toEqual({});
  });

  it("names the only slot off the leaving Server when every other Server answered", () => {
    const copies: ObservedCopy[] = [{ machine_id: id("a"), name: "data", role: "slot" }, { machine_id: id("b"), name: "data", role: "slot" }];
    expect(heldOff(answered(copies), servers, id("b"))).toEqual({ data: "fsn-a" });
    expect(heldOff(answered(copies, [id("b")]), servers, id("b"))).toEqual({ data: "fsn-a" });
  });

  it("names no slot when a third Server that may hold another did not answer", () => {
    expect(heldOff(answered([{ machine_id: id("a"), name: "data", role: "slot" }], [id("c")]), servers, id("b"))).toEqual({});
  });

  it("names no copy on the leaving Server", () => {
    expect(heldOff(answered([{ machine_id: id("b"), name: "data", role: "writer" }]), servers, id("b"))).toEqual({});
  });
});
