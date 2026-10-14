import { createdAt, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { bigint, boolean, index, integer, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";

export const githubInstallation = pgTable("github_installation", {
  installationId: integer("installation_id").primaryKey(),
  accountLogin: text("account_login").notNull(),
  accountType: text("account_type").notNull(),
  accountAvatarUrl: text("account_avatar_url"),
  createdAt,
  updatedAt,
});

export const organizationGithubInstallation = pgTable(
  "organization_github_installation",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    installationId: integer("installation_id")
      .notNull()
      .references(() => githubInstallation.installationId, { onDelete: "cascade" }),
    connectedByUserId: uuid("connected_by_user_id").references(() => user.id, { onDelete: "set null" }),
    connectedGithubLogin: text("connected_github_login").notNull(),
    connectedAt: timestamp("connected_at", { mode: "date", withTimezone: true }).defaultNow().notNull(),
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.installationId] }),
    index("organization_github_installation_installation_idx").on(table.installationId),
  ],
);

export const githubRepositoryCache = pgTable(
  "github_repository_cache",
  {
    installationId: integer("installation_id")
      .notNull()
      .references(() => githubInstallation.installationId, { onDelete: "cascade" }),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    name: text("name").notNull(),
    fullName: text("full_name").notNull(),
    defaultBranch: text("default_branch").notNull(),
    private: boolean("private").notNull(),
    htmlUrl: text("html_url").notNull(),
    repoUpdatedAt: timestamp("repo_updated_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    syncedAt: timestamp("synced_at", {
      mode: "date",
      withTimezone: true,
    })
      .defaultNow()
      .notNull(),
  },
  (table) => [
    primaryKey({ columns: [table.installationId, table.repositoryId] }),
    index("github_repository_cache_full_name_idx").on(table.fullName),
  ],
);
