import { sql } from "drizzle-orm";
import { check, jsonb, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt, type EncryptedSecretValue, type MachineId, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

/** An ingress Server's Machine id and public address. */
export type IngressServerAddress = { machineId: MachineId; address: string };

/**
 * The Organization's Cluster Domain, granted by Hosted DNS. Owned by the Organization, not a pairing:
 * it survives teardown and re-pairing, and cascades away only with the Organization.
 */
export const organizationClusterDomain = pgTable(
  "organization_cluster_domain",
  {
    organizationId: uuid("organization_id").primaryKey()
      .references(() => organization.id, { onDelete: "cascade" }),
    /** The Hosted DNS endpoint that granted the name; every later call for it goes there. */
    endpoint: text("endpoint").notNull(),
    name: text("name").notNull(),
    encryptedToken: jsonb("encrypted_token").notNull().$type<EncryptedSecretValue>(),
    reservedAt: timestamptz("reserved_at").notNull(),
    leaseRenewedAt: timestamptz("lease_renewed_at").notNull(),
    /** Null until the first successful records PUT. */
    recordsSyncedAt: timestamptz("records_synced_at"),
    /** The ingress Servers the last records PUT pointed the apex at. */
    recordAddresses: jsonb("record_addresses").notNull().default([]).$type<IngressServerAddress[]>(),
    /** Ingress Servers with a public IP that the last sync could not reach on port 80. */
    unreachable: jsonb("unreachable").notNull().default([]).$type<IngressServerAddress[]>(),
    /** The wildcard certificate for `*.name`: all three set together, or none. */
    encryptedCertificatePrivateKey: jsonb("encrypted_certificate_private_key").$type<EncryptedSecretValue>(),
    certificateChain: text("certificate_chain"),
    certificateNotAfter: timestamptz("certificate_not_after"),
    createdAt,
    updatedAt,
  },
  (table) => [
    check("organization_cluster_domain_record_addresses_check", sql`jsonb_typeof(${table.recordAddresses}) = 'array'`),
    check(
      "organization_cluster_domain_certificate_check",
      sql`num_nulls(${table.encryptedCertificatePrivateKey}, ${table.certificateChain}, ${table.certificateNotAfter}) in (0, 3)`,
    ),
  ],
);

export type OrganizationClusterDomain = typeof organizationClusterDomain.$inferSelect;
