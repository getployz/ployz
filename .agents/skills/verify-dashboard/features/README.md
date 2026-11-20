# Feature map

Each file covers one area of the dashboard: what is in it, how a user reaches it, the accessible names to drive it with, and its traps. The handles were recorded against the seed. If a name in a file no longer matches `snapshot -i`, the UI changed: fix the file in the same PR.

| Area | Seeded entry point |
| --- | --- |
| [Canvas and service inspector](canvas-and-inspector.md) | `/cloud/ada/shop/production` |
| [Unpublished changes and Deploy](changes-and-deploy.md) | `/cloud/ada/shop/production`, the **Details** button |
| [Branches](branches.md) | `/cloud/ada/shop/fix-api`, the **Sync** button |
| [Organization: projects, servers and settings](organization.md) | `/cloud/ada/~` |

Not mapped yet: an Environment's Logs, Analytics, History and Settings pages (`ENV/logs.tsx`, `ENV/analytics.tsx`, `ENV/history.tsx`, `ENV/settings.tsx`), PR Environments (`ENV/_canvas/pr-environments/`), Server detail (`~/servers/$serverId`) and the **Agent** panel. Read their source before driving them.

Paths below shorten `dashboard/src/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/` to `ENV/`.
