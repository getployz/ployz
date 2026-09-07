import type { RegisterRequest } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  heldMachineIds,
  registerThroughHeldList,
} from "#/modules/machines/enrollment.server";
import {
  makePloyzLayer,
  PloyzProviderError,
} from "#/modules/runtime/ployz.server";

const preferred = Schema.decodeUnknownSync(rustMachineIdSchema)("a".repeat(32));
const other = Schema.decodeUnknownSync(rustMachineIdSchema)("b".repeat(32));
const identity: RegisterRequest = {
  name: "node-1",
  storage: "none",
  public_key: Array.from<number>({ length: 32 }).fill(2),
  advertised_endpoints: ["10.0.0.1:51820"],
  public_ip: "203.0.113.10",
  runtime: {
    daemon_version: "",
    docker_version: "",
    hostname: "",
    architecture: "",
    os_pretty_name: "",
    kernel_version: "",
  },
};
const registered = {
  assigned_machine: other,
  visible_peers: [preferred],
  target_versions: {},
};

describe("registerThroughHeldList", () => {
  const pairing = "ppair_secret";
  const relayUrl = "https://relay.example.test";
  const bearer = "pdial_secret";
  const input = { relayUrl, bearer, pairing, held: [preferred], identity };

  it("Registers through a held Machine with Dial credential + pairing + machineId", async () => {
    const registeredCalls: unknown[] = [];
    const outcome = await Effect.runPromise(
      registerThroughHeldList(input).pipe(
        Effect.provide(
          makePloyzLayer({
            connect: async () => {
              throw new Error("unused");
            },
            register: async (
              receivedRelayUrl,
              receivedBearer,
              receivedPairing,
              machineId,
              request,
            ) => {
              registeredCalls.push({
                relayUrl: receivedRelayUrl,
                bearer: receivedBearer,
                pairing: receivedPairing,
                machineId,
                identity: request,
              });
              return registered;
            },
          }),
        ),
      ),
    );

    expect(registeredCalls).toEqual([
      { relayUrl, bearer, pairing, machineId: preferred, identity },
    ]);
    expect(outcome).toEqual({ kind: "registered", registration: registered });
    expect(JSON.stringify(outcome)).not.toMatch(/10\.\d+\.\d+\.\d+\/24/u);
  });

  it("skips an unreachable List entry and Joins through the next one", async () => {
    const tried: string[] = [];
    const outcome = await Effect.runPromise(
      registerThroughHeldList({
        relayUrl,
        bearer,
        pairing,
        held: [preferred, other],
        identity,
      }).pipe(
        Effect.provide(
          makePloyzLayer({
            connect: async () => {
              throw new Error("unused");
            },
            register: async (_relayUrl, _bearer, _pairing, machineId) => {
              tried.push(machineId);
              if (machineId === preferred) {
                throw new PloyzProviderError({
                  operation: "register held machine",
                  cause: new Error("Allocator is unreachable"),
                });
              }
              return registered;
            },
          }),
        ),
      ),
    );

    expect(tried).toEqual([preferred, other]);
    expect(outcome).toEqual({ kind: "registered", registration: registered });
  });

  it("returns not_yet when every Dial or forwarded Register is retryable Allocator not quiet", async () => {
    const outcome = await Effect.runPromise(
      registerThroughHeldList({
        relayUrl,
        bearer,
        pairing,
        held: [preferred, other],
        identity,
      }).pipe(
        Effect.provide(
          makePloyzLayer({
            connect: async () => {
              throw new Error("unused");
            },
            register: async () => {
              throw new PloyzProviderError({
                operation: "register held machine",
                cause: Object.assign(new Error("Allocator is not quiet"), {
                  retriable: true,
                }),
              });
            },
          }),
        ),
      ),
    );

    expect(outcome).toEqual({ kind: "not_yet" });
  });

  it("never assigns a Cloud /24", async () => {
    const outcome = await Effect.runPromise(
      registerThroughHeldList({
        relayUrl,
        bearer,
        pairing,
        held: [other],
        identity,
      }).pipe(
        Effect.provide(
          makePloyzLayer({
            connect: async () => {
              throw new Error("unused");
            },
            register: async () => registered,
          }),
        ),
      ),
    );

    expect(outcome).toEqual({ kind: "registered", registration: registered });
    expect(JSON.stringify(outcome)).not.toMatch(/\/24/u);
  });
});

describe("heldMachineIds", () => {
  it("keeps rows with a usable Machine id and drops the rest", () => {
    expect(
      heldMachineIds([
        { machineId: preferred },
        { machineId: "not-a-machine" },
        { nope: true },
        null,
        "string",
        { machineId: other },
      ]),
    ).toEqual([preferred, other]);
  });

  it("yields nothing when no row carries a usable id", () => {
    // Drives the indeterminate observation: entries exist, none is usable, so
    // the List is neither Dial-able nor evidence that nobody holds a Register.
    expect(heldMachineIds([{ machineId: "not-a-machine" }, {}])).toEqual([]);
  });
});
