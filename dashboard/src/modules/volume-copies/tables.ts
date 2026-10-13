import { index, integer, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt, type EncryptedSecretValue, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";
import type { SnapshotPolicy } from "./snapshot-schedule";

/** A Volume's automatic snapshots; a Volume without a row takes none. */
export const volumeSnapshotPolicy = pgTable(
  "volume_snapshot_policy",
  {
    volumeId: text("volume_id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    policy: jsonb("policy").$type<SnapshotPolicy>().notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [index("volume_snapshot_policy_environment_idx").on(table.organizationId, table.environmentId)],
);

/** A project's S3 bucket for offsite copies; the secret key is stored only encrypted. */
export const offsiteBucket = pgTable(
  "offsite_bucket",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    projectId: text("project_id").notNull(),
    endpoint: text("endpoint").notNull(),
    region: text("region").notNull(),
    bucket: text("bucket").notNull(),
    prefix: text("prefix").notNull(),
    accessKey: text("access_key").notNull(),
    encryptedSecretKey: jsonb("encrypted_secret_key").$type<EncryptedSecretValue>().notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    uniqueIndex("offsite_bucket_place_idx").on(
      table.organizationId,
      table.projectId,
      table.endpoint,
      table.bucket,
      table.prefix,
    ),
  ],
);

export const volumeOffsite = pgTable(
  "volume_offsite",
  {
    volumeId: text("volume_id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    bucketId: uuid("bucket_id")
      .notNull()
      .references(() => offsiteBucket.id, { onDelete: "restrict" }),
    keep: integer("keep").notNull().default(7),
    /** When a backup last finished uploading, as Cloud saw it end; null until the first. */
    uploadedAt: timestamp("uploaded_at", { mode: "date", withTimezone: true }),
    createdAt,
    updatedAt,
  },
  (table) => [index("volume_offsite_environment_idx").on(table.organizationId, table.environmentId)],
);
