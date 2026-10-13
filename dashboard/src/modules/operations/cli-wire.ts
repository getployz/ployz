import { Schema } from "effect";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";

/** `server rm`: the reset's DataLossConfirmation, the Volumes the user accepted losing. */
export const RemoveServer = Schema.Struct({ confirm_data_loss: Schema.Struct({ confirmed: Schema.Array(dataLossIdentitySchema) }) });

/** `server forget <server>`: nothing to confirm beyond the approval. */
export const ForgetServer = Schema.Struct({});

export const UpgradeServer = Schema.Struct({ channel: Schema.optional(Schema.String) });

/** `server forget` with no Server: the Organization's slug, as the user typed it. */
export const ForgetServers = Schema.Struct({ organization: Schema.String });

const RunEnvironment = Schema.Struct({ project: Schema.String, environment: Schema.String });
const Destination = Schema.String.check(Schema.isNonEmpty());

/** `volume mirror|sync|mirror rm|move|release|restore`, each one kind of `POST volumes/:id/runs`. */
export const VOLUME_RUN_BODIES = {
  mirror: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("mirror"), to: Destination }),
  sync: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("sync"), full: Schema.optional(Schema.Boolean) }),
  delete_mirror: Schema.Struct({
    environment: RunEnvironment,
    kind: Schema.Literal("delete_mirror"),
    slot: Schema.optional(Destination),
    confirm: Schema.optional(Schema.String),
  }),
  move: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("move"), to: Destination }),
  release: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("release") }),
  restore: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("restore"), from: Destination }),
} as const;

export const NewVolumeRun = Schema.Union(Object.values(VOLUME_RUN_BODIES));
