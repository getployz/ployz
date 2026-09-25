import type { MachineUpdate } from "@ployz/sdk";
import { Schema } from "effect";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());

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
});

export type ServerPolicyChange = typeof ServerPolicyChangeSchema.Type;

export const RequestServerPolicyChangeInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  machineId: NonEmptyString,
  change: ServerPolicyChangeSchema,
});

export type RequestServerPolicyChangeInput =
  typeof RequestServerPolicyChangeInput.Type;

/** The Engine's partial MachineUpdate for one Server Policy change. */
export function machineUpdateForPolicyChange(
  change: ServerPolicyChange,
): Partial<MachineUpdate> {
  const update: Partial<MachineUpdate> = {};
  if (change.acceptsBuilds !== undefined) {
    update.accepts_builds = change.acceptsBuilds;
  }
  if (change.buildConcurrency === "automatic") {
    update.build_concurrency = { action: "automatic" };
  } else if (change.buildConcurrency !== undefined) {
    update.build_concurrency = { action: "set", value: change.buildConcurrency };
  }
  return update;
}

/** Whether Runtime observation already shows every value of `change`. */
export function policyChangeObserved(
  observed: { acceptsBuilds: boolean; buildConcurrency: number | null },
  change: ServerPolicyChange,
): boolean {
  return (
    (change.acceptsBuilds === undefined ||
      change.acceptsBuilds === observed.acceptsBuilds) &&
    (change.buildConcurrency === undefined ||
      change.buildConcurrency === (observed.buildConcurrency ?? "automatic"))
  );
}
