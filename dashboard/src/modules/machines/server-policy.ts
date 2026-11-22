import type { MachineUpdate } from "@ployz/sdk";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { RegionSchema } from "#/modules/machines/server-regions";
import { REGION_LABEL } from "#/modules/runtime/runtime-watch-frame";

/** Builds a Server runs at once, as the Engine's `BuildConcurrency` bounds it. */
export const BuildConcurrencySchema = Schema.Int.check(
  Schema.isBetween({ minimum: 1, maximum: 255 }),
);

/** `automatic` clears the explicit value so the Engine derives it. */
export const BuildConcurrencyChangeSchema = Schema.Union([
  Schema.Literal("automatic"),
  BuildConcurrencySchema,
]);

export type BuildConcurrencyChange = typeof BuildConcurrencyChangeSchema.Type;

/** One Server Policy change. Omitted fields keep the Server's current value. */
export const ServerPolicyChangeSchema = Schema.Struct({
  acceptsBuilds: Schema.optionalKey(Schema.Boolean),
  buildConcurrency: Schema.optionalKey(BuildConcurrencyChangeSchema),
  /** Off is a cordon: nothing new starts on the Server, and what runs there stays until it's drained. */
  acceptsServices: Schema.optionalKey(Schema.Boolean),
  /** Only off: turning web traffic on also starts the Ingress Proxy, which the Server page switch or `ployz server set` does. */
  acceptsIngress: Schema.optionalKey(Schema.Literal(false)),
  /** Null clears the Server's region. */
  region: Schema.optionalKey(Schema.NullOr(RegionSchema)),
});

export type ServerPolicyChange = typeof ServerPolicyChangeSchema.Type;

export const RequestServerPolicyChangeInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  machineId: rustMachineIdSchema,
  change: ServerPolicyChangeSchema,
});

export type RequestServerPolicyChangeInput =
  typeof RequestServerPolicyChangeInput.Type;

/** A change that sets nothing changes nothing; the request refuses it. */
export const isEmptyPolicyChange = (change: ServerPolicyChange) =>
  change.acceptsBuilds === undefined && change.buildConcurrency === undefined && change.acceptsServices === undefined &&
  change.acceptsIngress === undefined && change.region === undefined;

/** The Engine's partial MachineUpdate for one Server Policy change. */
export function machineUpdateForPolicyChange(
  change: ServerPolicyChange,
): Partial<MachineUpdate> {
  const update: Partial<MachineUpdate> = {};
  if (change.acceptsBuilds !== undefined) {
    update.accepts_builds = change.acceptsBuilds;
  }
  if (change.acceptsServices !== undefined) {
    update.accepts_services = change.acceptsServices;
  }
  if (change.acceptsIngress !== undefined) {
    update.accepts_ingress = change.acceptsIngress;
  }
  if (change.buildConcurrency === "automatic") {
    update.build_concurrency = { action: "automatic" };
  } else if (change.buildConcurrency !== undefined) {
    update.build_concurrency = { action: "set", value: change.buildConcurrency };
  }
  if (change.region !== undefined) {
    update.label_changes = { [REGION_LABEL]: change.region };
  }
  return update;
}

/** Whether Runtime observation already shows every value of `change`. */
export function policyChangeObserved(
  observed: {
    acceptsBuilds: boolean;
    acceptsServices: boolean;
    acceptsIngress: boolean;
    buildConcurrency: number | null;
    region: string | null;
  },
  change: ServerPolicyChange,
): boolean {
  return (
    (change.acceptsBuilds === undefined ||
      change.acceptsBuilds === observed.acceptsBuilds) &&
    (change.acceptsServices === undefined ||
      change.acceptsServices === observed.acceptsServices) &&
    (change.acceptsIngress === undefined ||
      change.acceptsIngress === observed.acceptsIngress) &&
    (change.buildConcurrency === undefined ||
      change.buildConcurrency === (observed.buildConcurrency ?? "automatic")) &&
    (change.region === undefined || change.region === observed.region)
  );
}
