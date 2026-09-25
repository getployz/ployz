import { sql } from "drizzle-orm";
import { check, jsonb, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt, type EncryptedSecretValue, type MachineId, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

/** An ingress Server's Machine id and public address. */
export type IngressServerAddress = { machineId: MachineId; address: string };

/**
 * What the last sync found about the Servers that carry the Cluster Domain's traffic:
 * - `no_servers`: no Cluster is paired, or its runtime frame has no Machines.
 * - `no_public_ip`: no ingress Server has a public IP.
 * - `probed`: ingress Servers were probed; `unreachable` lists those that did not answer on port 80.
 */
export type ClusterDomainTraffic =
  | { kind: "no_servers" }
  | { kind: "no_public_ip" }
  | { kind: "probed"; unreachable: IngressServerAddress[] };

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
    /**
     * What the last sync found about the ingress Servers. Null when never checked, or when the last
     * check could not read a paired Cluster's frame: an offline Cluster reads as ready.
     */
    traffic: jsonb("traffic").$type<ClusterDomainTraffic>(),
    /** When a sync last recorded what it found about the Servers, right after its probe. Null until the first one. */
    checkedAt: timestamptz("checked_at"),
    /** The wildcard certificate for `*.name`: all three set together, or none. */
    encryptedCertificatePrivateKey: jsonb("encrypted_certificate_private_key").$type<EncryptedSecretValue>(),
    certificateChain: text("certificate_chain"),
    certificateNotAfter: timestamptz("certificate_not_after"),
    createdAt,
    updatedAt,
  },
  (table) => [
    check(
      "organization_cluster_domain_traffic_check",
      sql`${table.traffic} is null or ${table.traffic}->>'kind' in ('no_servers', 'no_public_ip', 'probed')`,
    ),
    check(
      "organization_cluster_domain_certificate_check",
      sql`num_nulls(${table.encryptedCertificatePrivateKey}, ${table.certificateChain}, ${table.certificateNotAfter}) in (0, 3)`,
    ),
  ],
);

export type OrganizationClusterDomain = typeof organizationClusterDomain.$inferSelect;
