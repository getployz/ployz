import { Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { ApprovalInput } from "#/modules/approval-session/approvals-input";

export const SealingPublicKey = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9+/]{43}=$/));

const RECOVERY_BACKUP_VERSION = 1;

/** A Sealing Key wrapped under a recovery code: the 77-byte v1 envelope, which Cloud stores and can't open. */
export const RecoveryBackup = Schema.String.check(
  Schema.isPattern(/^[A-Za-z0-9+/]{103}=$/),
  Schema.makeFilter((value) => btoa(atob(value)) === value && atob(value).charCodeAt(0) === RECOVERY_BACKUP_VERSION, {
    message: "backup must be a v1 recovery backup.",
  }),
);

export const RecoveryDigest = Schema.String.check(Schema.isPattern(/^[0-9a-f]{64}$/));

export const CliReadSealingRecoveryInput = Schema.Struct({ organizationId: Uuid });
export type CliReadSealingRecoveryInput = typeof CliReadSealingRecoveryInput.Type;

export const CliRotateSealingRecoveryInput = Schema.Struct({
  organizationId: Uuid,
  publicKey: SealingPublicKey,
  currentDigest: Schema.NullOr(RecoveryDigest),
  backup: RecoveryBackup,
});
export type CliRotateSealingRecoveryInput = typeof CliRotateSealingRecoveryInput.Type;

export const CliReadRecipientRecoveryInput = Schema.Struct({ organizationId: Uuid });
export type CliReadRecipientRecoveryInput = typeof CliReadRecipientRecoveryInput.Type;

/**
 * The recipient key a Cluster's activation added, wrapped under its own recovery code, with the signed rotation that
 * named it. A recovering Server checks the rotation; Cloud only bounds its size.
 */
export const CliRotateRecipientRecoveryInput = Schema.Struct({
  organizationId: Uuid,
  publicKey: SealingPublicKey,
  rotation: ApprovalInput,
  currentDigest: Schema.NullOr(RecoveryDigest),
  backup: RecoveryBackup,
});
export type CliRotateRecipientRecoveryInput = typeof CliRotateRecipientRecoveryInput.Type;
export type RecipientRotation = CliRotateRecipientRecoveryInput["rotation"];
