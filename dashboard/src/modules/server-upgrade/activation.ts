/**
 * Activation as the Servers page shows it: each Server's own account of where it stands, and whether the dashboard may
 * offer the passkey path. Cloud relays every account, so the gate here only decides what the page offers. What holds is
 * that every Server checks the set a passkey answers against its own trusted snapshot, and refuses a legacy Member.
 */
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { requirements } from "#/modules/runtime/daemon-compat";
import { SealingPublicKey } from "#/modules/machines/sealing-key";

/** What a Server advertises once it can activate: it gates every signed route and keeps a trusted snapshot. */
export const ENFORCED_CAPABILITY = requirements.enforced.capability;

export const ACTIVATION_STATES = ["awaiting", "activating", "activated"] as const;
export type ActivationState = (typeof ACTIVATION_STATES)[number];

/** A Member of a starting set, as a Server reports itself: what every Server compares. Its mesh key is 32 bytes. */
export type ActivationMember = { readonly id: string; readonly public_key: readonly number[]; readonly subnet: string };

export type LegacyKey = { readonly code: string; readonly kind: "device" | "passkey"; readonly account: string | null };

/** One Server's own report of its activation. */
export type ActivationReport = {
  readonly state: ActivationState;
  readonly operation: string | null;
  readonly legacyMembers: readonly string[];
  readonly legacyKeys: readonly LegacyKey[];
  /** The Private Access router it keeps from before activation, and the tailnet its node joined, when it names one. */
  readonly privateAccess: { readonly tailnet: string | null } | null;
};

/** One Server as Cloud read it through Inspect. A Server that didn't answer claims nothing. */
export type ActivationServer = {
  readonly id: string;
  readonly name: string;
  readonly answered: boolean;
  readonly enforces: boolean;
  readonly member: ActivationMember | null;
  /** The Cluster's identity: the Sealing Key it holds. */
  readonly identity: string | null;
  /** The key new values seal to once a person named one at activation. */
  readonly recipient: string | null;
  /** Absent from a Ployz older than 0.4. */
  readonly activation: ActivationReport | null;
};

/** Why the dashboard can't activate, each naming the Servers it's about. */
export type ActivationBlocker =
  | { readonly kind: "unanswered"; readonly servers: readonly string[] }
  | { readonly kind: "older"; readonly servers: readonly string[] }
  | { readonly kind: "legacy-members"; readonly servers: readonly string[]; readonly members: readonly string[] }
  | { readonly kind: "legacy-keys"; readonly servers: readonly string[]; readonly keys: readonly LegacyKey[] }
  | { readonly kind: "partial"; readonly servers: readonly string[] }
  | { readonly kind: "no-cluster-key" };

export type ActivationGate =
  /** No Server runs a Ployz that can activate: there's nothing to say yet. */
  | { readonly kind: "none" }
  | { readonly kind: "offer"; readonly cluster: string; readonly members: readonly ActivationMember[] }
  | { readonly kind: "activating"; readonly servers: readonly string[] }
  /** Every Server activated under one set, but none holds a new key yet: values still seal to the Cluster's own. */
  | { readonly kind: "keyless"; readonly cluster: string; readonly operation: string; readonly members: readonly ActivationMember[] }
  | { readonly kind: "activated" }
  | { readonly kind: "blocked"; readonly blockers: readonly ActivationBlocker[] };

const names = (servers: readonly ActivationServer[]) => servers.map((server) => server.name);

/** Every Server activated: whether one still needs the new key, which the page then offers to set. */
function settledGate(servers: readonly ActivationServer[]): ActivationGate {
  if (servers.some((server) => server.recipient !== null)) return { kind: "activated" };
  const operations = new Set(servers.map((server) => server.activation?.operation ?? null));
  const identities = new Set(servers.map((server) => server.identity));
  const [operation] = operations;
  const [cluster] = identities;
  const members = servers.flatMap((server) => server.member === null ? [] : [server.member]);
  if (operations.size !== 1 || identities.size !== 1 || operation == null || cluster == null || members.length !== servers.length) {
    return { kind: "activated" };
  }
  return { kind: "keyless", cluster, operation, members };
}

/**
 * What the page offers. It offers the passkey path only when every Server answered, runs a Ployz that can activate,
 * awaits activation, reports no legacy Member or key, and holds the one Sealing Key. A pending activation reads as
 * activating; every Server activated reads as activated, or as keyless until one holds the new key. Otherwise it names
 * why, for the CLI.
 */
export function activationGate(servers: readonly ActivationServer[]): ActivationGate {
  if (!servers.some((server) => server.enforces)) return { kind: "none" };
  const activating = servers.filter((server) => server.activation?.state === "activating");
  if (activating.length > 0) return { kind: "activating", servers: names(activating) };
  if (servers.every((server) => server.activation?.state === "activated")) return settledGate(servers);

  const byId = new Map(servers.map((server) => [server.id, server.name]));
  const blockers: ActivationBlocker[] = [];
  const unanswered = servers.filter((server) => !server.answered);
  if (unanswered.length > 0) blockers.push({ kind: "unanswered", servers: names(unanswered) });
  const answered = servers.filter((server) => server.answered);
  const older = answered.filter((server) => !server.enforces || server.activation === null);
  if (older.length > 0) blockers.push({ kind: "older", servers: names(older) });
  const activated = answered.filter((server) => server.activation?.state === "activated");
  if (activated.length > 0) blockers.push({ kind: "partial", servers: names(activated) });
  const withMembers = answered.filter((server) => (server.activation?.legacyMembers.length ?? 0) > 0);
  if (withMembers.length > 0) {
    const members = [...new Set(withMembers.flatMap((server) => server.activation?.legacyMembers ?? []))];
    blockers.push({ kind: "legacy-members", servers: names(withMembers), members: members.map((id) => byId.get(id) ?? id) });
  }
  const withKeys = answered.filter((server) => (server.activation?.legacyKeys.length ?? 0) > 0);
  if (withKeys.length > 0) {
    const keys = new Map(withKeys.flatMap((server) => server.activation?.legacyKeys ?? []).map((key) => [key.code, key]));
    blockers.push({ kind: "legacy-keys", servers: names(withKeys), keys: [...keys.values()] });
  }
  const identities = new Set(answered.map((server) => server.identity));
  const [cluster] = identities;
  if (identities.size !== 1 || cluster === null || cluster === undefined) blockers.push({ kind: "no-cluster-key" });

  const members = answered.flatMap((server) => server.member === null ? [] : [server.member]);
  if (members.length !== answered.length && !blockers.some((blocker) => blocker.kind === "unanswered")) {
    blockers.push({ kind: "unanswered", servers: names(answered.filter((server) => server.member === null)) });
  }
  if (blockers.length > 0 || cluster === null || cluster === undefined) return { kind: "blocked", blockers };
  return { kind: "offer", cluster, members };
}

/** What a passkey answered, as a browser relays it. Servers verify every byte; Cloud bounds the size. */
const Base64Url = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{1,8192}$/u));
export const PasskeyAnswered = Schema.Struct({
  body: Schema.String.check(Schema.isMaxLength(64_000)),
  signatures: Schema.Tuple([]),
  assertions: Schema.Array(Schema.Struct({
    credential_id: Base64Url,
    authenticator_data: Base64Url,
    client_data_json: Base64Url,
    signature: Base64Url,
  })).check(Schema.isMinLength(1), Schema.isMaxLength(1)),
});
export type PasskeyAnswered = typeof PasskeyAnswered.Type;

const OperationId = Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/u));
const MemberBody = Schema.Struct({ id: rustMachineIdSchema, public_key: Schema.Array(Schema.Int), subnet: Schema.String });

/** What Cloud reads of a starting set's body to know which Servers to hand it to. Each Server reads the rest. */
export const StartingSetRead = Schema.fromJsonString(Schema.Struct({
  operation: OperationId,
  members: Schema.Array(MemberBody).check(Schema.isMinLength(1), Schema.isMaxLength(256)),
}));

/** What Cloud reads of a rotation's body to know where to install it. Each Server verifies the rest. */
export const RotationRead = Schema.fromJsonString(Schema.Struct({
  cluster: SealingPublicKey,
  operation: OperationId,
  recipient: SealingPublicKey,
  members: Schema.Array(MemberBody).check(Schema.isMinLength(1), Schema.isMaxLength(256)),
}));

export const ActivateServersInput = Schema.Struct({ organizationSlug: Schema.NonEmptyString, startingSet: PasskeyAnswered });
export type ActivateServersInput = typeof ActivateServersInput.Type;

export const GenerateRecipientInput = Schema.Struct({ organizationSlug: Schema.NonEmptyString, operation: OperationId });
export type GenerateRecipientInput = typeof GenerateRecipientInput.Type;

export const InstallRecipientInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  rotation: PasskeyAnswered,
  /** The Server that generated the recipient key, which installs it first and shares it with the rest. */
  generator: rustMachineIdSchema,
});
export type InstallRecipientInput = typeof InstallRecipientInput.Type;

/** What became of one Server's activation, in words the page shows as they are. */
export type ServerActivation =
  | { readonly id: string; readonly name: string; readonly kind: "activated"; readonly privateAccess: { readonly tailnet: string | null } | null }
  | { readonly id: string; readonly name: string; readonly kind: "refused"; readonly why: string };

/** The recipient key a Server generated for the second prompt, and which Server holds it. */
export type Generated = { readonly generator: string; readonly recipient: string };

/** What became of the recipient key on each Server that activated. */
export type RecipientInstall =
  | { readonly id: string; readonly name: string; readonly kind: "installed" }
  | { readonly id: string; readonly name: string; readonly kind: "refused"; readonly why: string };
