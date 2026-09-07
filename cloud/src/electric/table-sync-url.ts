import { createIsomorphicFn } from "@tanstack/react-start";
import type { PloyzTableName } from "#/electric/synced-tables.server";

export const getTableSyncBaseUrl = createIsomorphicFn()
  .client(() => document.baseURI)
  .server(async () => {
    const [{ Effect }, { AppConfig }, { runAppEffect }] = await Promise.all([
      import("effect"),
      import("#/server/config.server"),
      import("#/server/run.server"),
    ]);
    return runAppEffect(Effect.map(AppConfig, (config) => config.app.url.href));
  });

const getClientTableSyncBaseUrl = createIsomorphicFn()
  .client(() => document.baseURI)
  .server(() => {
    throw new Error("Server collection loaders must provide the AppConfig URL");
  });

export function tableSyncUrl(
  table: PloyzTableName,
  searchParams: Record<string, string> = {},
  baseUrl = getClientTableSyncBaseUrl(),
) {
  const url = new URL(
    `/api/shapes/${encodeURIComponent(table)}`,
    baseUrl,
  );
  for (const [key, value] of Object.entries(searchParams)) {
    url.searchParams.set(key, value);
  }
  return url.toString();
}
