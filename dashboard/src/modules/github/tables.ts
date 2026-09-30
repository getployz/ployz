import { createdAt, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { bigint, boolean, index, integer, pgTable, primaryKey, text, timestamp, unique, uuid } from "drizzle-orm/pg-core";

export const githubInstallation = pgTable(
  "github_installation",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    installationId: integer("installation_id").notNull(),
    accountLogin: text("account_login").notNull(),
    accountType: text("account_type").notNull(),
    accountAvatarUrl: text("account_avatar_url"),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.userId, table.installationId),
    index("github_installation_user_idx").on(table.userId),
    index("github_installation_installation_id_idx").on(table.installationId),
  ],
);

export const githubRepositoryCache = pgTable(
  "github_repository_cache",
  {
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    installationId: integer("installation_id").notNull(),
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
    primaryKey({
      columns: [table.userId, table.installationId, table.repositoryId],
    }),
    index("github_repository_cache_installation_idx").on(table.installationId),
    index("github_repository_cache_user_idx").on(table.userId),
    index("github_repository_cache_full_name_idx").on(table.fullName),
  ],
);
