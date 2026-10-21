import { Schema } from "effect";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import type { VolumeRunInput, VolumeRunRequestKind } from "#/modules/volume-run/volume-run";

/** The name the CLI resolved the Server by and showed, which its run keeps. An older CLI sends none. */
const shownName = { server_name: Schema.optional(Schema.String) };

/** `server rm`: the reset's DataLossConfirmation, the Volumes the user accepted losing. */
export const RemoveServer = Schema.Struct({ confirm_data_loss: Schema.Struct({ confirmed: Schema.Array(dataLossIdentitySchema) }), ...shownName });

/** `server forget <server>`: nothing to confirm beyond the approval. */
export const ForgetServer = Schema.Struct(shownName);

export const DrainServer = Schema.Struct(shownName);

export const UpgradeServer = Schema.Struct({ channel: Schema.optional(Schema.String), ...shownName });

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
  /**
   * `mirror rm --if-old-copy` sends `delete_old_copy`: the slot is deleted only while it is still an old copy. It is a
   * kind of its own so a Cloud from before it refuses the body instead of dropping the restriction.
   */
  delete_mirror: Schema.Struct({
    environment: RunEnvironment,
    kind: Schema.Literals(["delete_mirror", "delete_old_copy"]),
    slot: Schema.optional(Destination),
    confirm: Schema.optional(Schema.String),
  }),
  move: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("move"), to: Destination }),
  release: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("release") }),
  restore: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("restore"), from: Destination }),
  resize: Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("resize"), maximum_bytes: MaximumBytes }),
} as const satisfies Record<Exclude<VolumeRunRequestKind, "seed">, Schema.Top>;

export const NewVolumeRun = Schema.Union(Object.values(VOLUME_RUN_BODIES));

/** The run a CLI body asks for. A bare `mirror rm` deletes the slot whichever role it holds. */
export function volumeRunInput(body: typeof NewVolumeRun.Type): VolumeRunInput {
  switch (body.kind) {
    case "mirror":
      return { kind: "mirror", args: { to: body.to } };
    case "sync":
      return { kind: "sync", args: { full: true } };
    case "delete_mirror":
    case "delete_old_copy":
      return { kind: "delete_mirror", args: { slot: body.slot ?? null, confirmed_name: body.confirm ?? null, role: body.kind === "delete_old_copy" ? "old_copy" : "any" } };
    case "move":
      return { kind: "move", args: { to: body.to } };
    case "release":
      return { kind: "release", args: {} };
    case "restore":
      return { kind: "restore", args: { from: body.from } };
    case "resize":
      return { kind: "resize", args: { maximum_bytes: body.maximum_bytes } };
  }
}

/** `volume sync` without `--full`: the mirror sync, answered once the mirror has the pull. */
export const SyncVolume = Schema.Struct({ environment: RunEnvironment });
