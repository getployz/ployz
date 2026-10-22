import { Schema } from "effect";

export const SealingPublicKey = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9+/]{43}=$/));

const RECOVERY_BACKUP_VERSION = 1;

/** A Sealing Key wrapped under a recovery code: the 77-byte v1 envelope, which Cloud stores and can't open. */
export const RecoveryBackup = Schema.String.check(
  Schema.isPattern(/^[A-Za-z0-9+/]{103}=$/),
  Schema.makeFilter((value) => btoa(atob(value)) === value && atob(value).charCodeAt(0) === RECOVERY_BACKUP_VERSION, {
    message: "backup must be a v1 recovery backup.",
  }),
);
