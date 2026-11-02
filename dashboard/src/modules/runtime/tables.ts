import type { RemovalEndpoint } from "#/modules/machines/pairing-removal";
import { createdAt, type EncryptedSecretValue, type MachineId, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";
import { sql } from "drizzle-orm";
import { check, jsonb, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";

/** One org = one rust cluster. Pairing secret is per-tenant. */
export const organizationPairing = pgTable(
  "organization_pairing",
  {
    organizationId: uuid("organization_id")
      .primaryKey()
      .references(() => organization.id, { onDelete: "cascade" }),
    encryptedPairingSecret: jsonb("encrypted_pairing_secret")
      .notNull()
      .$type<EncryptedSecretValue>(),
    removalStartedAt: timestamp("removal_started_at", { mode: "date", withTimezone: true }),
    removalEndpoints: jsonb("removal_endpoints").$type<readonly RemovalEndpoint[] | null>(),
    removalAnsweredAt: timestamp("removal_answered_at", { mode: "date", withTimezone: true }),
    founderPublicKey: text("founder_public_key"),
    founderClaimMachineId: text("founder_claim_machine_id").notNull().$type<MachineId>(),
    /** The hash of the token that acquired the Founding Claim; null for a claim from before Cloud kept it. */
    founderClaimTokenHash: text("founder_claim_token_hash"),
    founderMachineId: text("founder_machine_id").$type<MachineId>(),
    /** The founder publication verifying its Server now, and when its turn lapses if it never finishes. */
    founderPublication: uuid("founder_publication"),
    founderPublicationUntil: timestamp("founder_publication_until", { mode: "date", withTimezone: true }),
    createdAt,
    updatedAt,
  },
  (table) => [
    check("organization_pairing_removal_shape_check", sql`
      (${table.removalStartedAt} is null and ${table.removalEndpoints} is null)
      or (${table.removalStartedAt} is not null and ${table.removalEndpoints} is not null and jsonb_typeof(${table.removalEndpoints}) = 'array')
    `),
    check(
      "organization_pairing_state_check",
      sql`${table.founderPublicKey} is not null or ${table.founderMachineId} is not null`,
    ),
    check(
      "organization_pairing_founder_claim_machine_id_check",
      sql`${table.founderClaimMachineId} ~ '^[0-9a-f]{32}$'`,
    ),
    check(
      "organization_pairing_founder_machine_id_check",
      sql`${table.founderMachineId} is null or ${table.founderMachineId} ~ '^[0-9a-f]{32}$'`,
    ),
  ],
);
