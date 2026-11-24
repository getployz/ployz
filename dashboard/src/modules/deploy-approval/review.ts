import type { DestructiveEffect, DestructiveKind, DiffView } from "@ployz/sdk";

/** A write into the Protected Environment `environment` by someone who can't protect it: an owner or admin decides it. */
export type Guarded = { environment: string };

/** A redeploy as the Store's review binds it: the Deployment it ships again, by number, and the newer ones it rolls back. */
export type Redeploy = { deployment: number; rolls_back: number[] };

/** What the Store asks a person to review before a write runs: the Destructive Effects and the diff they come from. */
export type StoreReview = { effects: DestructiveEffect[]; diff: DiffView; protected?: Guarded; redeploy?: Redeploy; prunes?: true; dropped?: string[] };

/** A full Deploy that also removes every other Service its Environment runs, which only the Servers can name. */
export const prunes = (review: object) => "diff" in review && "prunes" in review && review.prunes === true;

/** Why a pruning Deploy couldn't check every Server: the ones large packets don't reach. Null when it doesn't say. */
export function droppedLine(review: object) {
  if (!prunes(review) || !("dropped" in review) || !Array.isArray(review.dropped) || review.dropped.length === 0) return null;
  return `Couldn't check every server: large packets to ${review.dropped.join(", ")} are dropped on the mesh path`;
}

const destroys = {
  removes_service: { verb: "Removes", noun: "service", after: "" },
  deletes_volume: { verb: "Deletes", noun: "volume", after: ". Not recoverable." },
  detaches_volume: { verb: "Detaches", noun: "volume", after: "" },
  removes_domain: { verb: "Removes", noun: "domain", after: "" },
  removes_server: { verb: "Removes", noun: "server", after: "" },
  removes_member: { verb: "Removes", noun: "member", after: "" },
  changes_role: { verb: "Changes", noun: "role of", after: "" },
  revokes_invite: { verb: "Revokes", noun: "invite for", after: "" },
  stops_service: { verb: "Stops", noun: "service", after: " until started again" },
  restarts_service: { verb: "Restarts", noun: "service", after: "" },
  unprotects_environment: { verb: "Unprotects", noun: "environment", after: "" },
  removes_organization: { verb: "Removes", noun: "organization", after: ". Not recoverable." },
  restores_volume: { verb: "Restores", noun: "volume", after: "" },
} satisfies Record<DestructiveKind, { verb: string; noun: string; after: string }>;

/**
 * One thing a plan destroys, worded as a card leads with it: by name, or as "a volume" when it has none. A Save
 * destroys nothing itself, so its lines say the next Deploy does.
 */
export function destroyedLine(effect: DestructiveEffect, command = "admit") {
  const { verb: does, noun, after } = destroys[effect.kind];
  const save = command === "publish";
  const verb = save ? `Next deploy ${does.toLowerCase()}` : does;
  const end = save ? "." : after;
  const mount = effect.kind === "detaches_volume" ? effect.path.split(".mounts.") : [];
  const tail = mount.length === 2 ? ` from ${mount[0]}${end}` : end;
  return effect.name === undefined ? { verb: `${verb} a ${noun}`, name: null, after: tail } : { verb: `${verb} ${noun}`, name: effect.name, after: tail };
}
