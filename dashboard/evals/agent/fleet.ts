import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import type { Connection } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";

const MACHINES = new URL("./machines.json", import.meta.url);

const FleetFile = Schema.Struct({
  machines: Schema.Array(Schema.Struct({ name: Schema.String, id: rustMachineIdSchema, online: Schema.Boolean })),
});

/** The fake fleet's Machines by name, as machines.json describes them. */
export const MACHINES_BY_NAME = Object.fromEntries(
  Schema.decodeUnknownSync(Schema.fromJsonString(FleetFile))(readFileSync(MACHINES, "utf8")).machines.map((machine) => [machine.name, machine]),
);

/** A Volume copy on one Machine, by its Namespace and name. */
type Copy = { readonly namespace: string; readonly volume: string; readonly machine: string };

/** A fault the fleet injects in an effect, after the ledger recorded the request. */
export type Fault =
  /** Apply the `nth` matching request, then answer Unavailable as if the reply was lost after commit. */
  | { readonly fault: "drop_reply"; readonly machine: string; readonly command: string; readonly nth: number }
  /** Apply the matching request, then close the Machine's sockets: it goes offline. */
  | { readonly fault: "disconnect_after"; readonly machine: string; readonly command: string };

/** What a task changes on top of machines.json before the trial. */
export type FleetScenario = {
  readonly mirrors?: ReadonlyArray<Copy>;
  readonly drop_volumes?: ReadonlyArray<Copy>;
  readonly faults?: ReadonlyArray<Fault>;
};

/** A change sent to a running fleet. */
export type Control = { readonly online: string } | { readonly offline: string };

const LedgerEntry = Schema.Struct({
  seq: Schema.Number,
  machine: Schema.String,
  command: Schema.String,
  container: Schema.optional(Schema.NullOr(Schema.String)),
  /** The request's payload, kept to the fields that name what it acts on. `container` names a container instead. */
  request: Schema.NullOr(Schema.Struct({ attempt_id: Schema.optionalKey(Schema.String), name: Schema.optionalKey(Schema.String) })),
});
/** One request a Machine of the fleet received, recorded before its effect. */
export type LedgerEntry = typeof LedgerEntry.Type;

export type Fleet = {
  /** Every Machine's connection, offline ones included, as Cloud would dial them. */
  readonly connections: ReadonlyArray<Connection>;
  readonly control: (change: Control) => Effect.Effect<void>;
  /** Every request so far, in order. */
  readonly ledger: Effect.Effect<ReadonlyArray<LedgerEntry>>;
};

/** `PLOYZ_FAKE_FLEET`, or the debug build Cargo leaves in its target directory. */
const binary = () => process.env["PLOYZ_FAKE_FLEET"]
  ?? join(process.env["CARGO_TARGET_DIR"] ?? new URL("../../../core/target", import.meta.url).pathname, "debug", "ployz-fake-fleet");

/**
 * The fake fleet of machines.json with `scenario` applied, serving each online Machine on a unix socket until the
 * scope closes. Build it with `cargo build -p ployz-testkit --bin ployz-fake-fleet`.
 */
export const startFleet = Effect.fn("Eval.fleet")(function* (scenario: FleetScenario = {}) {
  const dir = yield* Effect.acquireRelease(
    Effect.sync(() => mkdtempSync(join(tmpdir(), "ployz-fleet-"))),
    (path) => Effect.sync(() => rmSync(path, { recursive: true, force: true })),
  );
  const ledgerPath = join(dir, "ledger.jsonl");
  const child = yield* Effect.acquireRelease(
    Effect.sync(() => spawn(binary(), [
      "--machines", MACHINES.pathname, "--sockets", dir, "--ledger", ledgerPath, "--scenario", JSON.stringify(scenario),
    ], { stdio: ["pipe", "pipe", "inherit"] })),
    (process) => Effect.sync(() => process.kill()),
  );
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const next = Effect.tryPromise({
    try: async () => {
      const line = await lines.next();
      if (line.done === true) throw new Error(`${binary()} exited before answering`);
      return line.value;
    },
    catch: (cause) => cause,
  }).pipe(Effect.orDie);
  const ready = yield* Effect.raceFirst(next, Effect.fromNullishOr(null).pipe(Effect.delay("10 seconds"), Effect.orDie, Effect.as("")));
  if (ready.trim() !== JSON.stringify({ ready: true })) return yield* Effect.die(`${binary()} said ${ready || "nothing"} instead of ready`);
  const fleet: Fleet = {
    connections: Object.values(MACHINES_BY_NAME).map((machine) => ({ unix: join(dir, `${machine.id}.sock`), machine_id: machine.id })),
    control: (change) => Effect.gen(function* () {
      child.stdin.write(`${JSON.stringify(change)}\n`);
      const answer = yield* next;
      if (answer.trim() !== JSON.stringify({ ok: true })) return yield* Effect.die(`the fleet answered ${answer} to ${JSON.stringify(change)}`);
    }),
    ledger: Effect.sync(() => {
      const text = (() => {
        try {
          return readFileSync(ledgerPath, "utf8");
        } catch {
          return "";
        }
      })();
      return text.split("\n").filter((line) => line.trim() !== "").map((line) => Schema.decodeUnknownSync(Schema.fromJsonString(LedgerEntry))(line));
    }),
  };
  return fleet;
});
