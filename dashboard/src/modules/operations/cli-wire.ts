import { Schema } from "effect";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import type { VolumeRunRequestKind } from "#/modules/volume-run/volume-run";

/** `server rm`: the reset's DataLossConfirmation, the Volumes the user accepted losing. */
export const RemoveServer = Schema.Struct({ confirm_data_loss: Schema.Struct({ confirmed: Schema.Array(dataLossIdentitySchema) }) });

/** `server forget <server>`: nothing to confirm beyond the approval. */
export const ForgetServer = Schema.Struct({});

export const UpgradeServer = Schema.Struct({ channel: Schema.optional(Schema.String) });

/** `server forget` with no Server: the Organization's slug, as the user typed it. */
export const ForgetServers = Schema.Struct({ organization: Schema.String });

const RunEnvironment = Schema.Struct({ project: Schema.String, environment: Schema.String });
const Destination = Schema.String.check(Schema.isNonEmpty());

/** A byte count the CLI sends: a positive whole number JSON carries exactly. */
export const MaximumBytes = Schema.Number.check(Schema.isInt(), Schema.isGreaterThan(0), Schema.isLessThanOrEqualTo(Number.MAX_SAFE_INTEGER));

/**
 * `volume mirror|sync|mirror rm|move|release|restore` and `volume set --size` on a deployed Volume, each one kind of
 * `POST volumes/:id/runs`. Keyed by every requested kind but `seed`, which only Cloud starts, so a new kind fails
 * to compile here until it says whether the CLI may ask for it.
 */
export const VOLUME_RUN_BODIES = {
  mirror: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("mirror"), to: Destination }),
  sync: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("sync"), full: Schema.Literal(true) }),
  delete_mirror: Schema.Struct({
    environment: RunEnvironment,
    kind: Schema.Literal("delete_mirror"),
    slot: Schema.optional(Destination),
    confirm: Schema.optional(Schema.String),
  }),
  move: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("move"), to: Destination }),
  release: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("release") }),
  restore: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("restore"), from: Destination }),
  resize: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("resize"), maximum_bytes: MaximumBytes }),
} as const satisfies Record<Exclude<VolumeRunRequestKind, "seed">, Schema.Top>;

export const NewVolumeRun = Schema.Union(Object.values(VOLUME_RUN_BODIES));

/** `volume sync` without `--full`: the mirror sync, answered once the mirror has the pull. */
export const SyncVolume = Schema.Struct({ environment: RunEnvironment });
