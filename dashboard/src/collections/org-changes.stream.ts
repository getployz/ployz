import { linkOptions } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { useEffect } from "react";
import { orgStoreTables } from "./collections";
import type { Invalidate } from "./change-log/declare";
import { changeNameSchema, type ChangeName } from "./read.contract";
import { orgChanges, orgCollections } from "./registry";
import type { CollectionScope } from "./scope";
import { useCollectionScope } from "./use-collection-scope";
import { refetchStoreViews } from "#/modules/config-store/store-view.queries";
import { liveStream } from "#/lib/live.stream";
import { storeViewNames, type StoreViewName } from "#/modules/config-store/store.change-sources";

const orgChangesEventSchema = Schema.Struct({ collections: Schema.Array(changeNameSchema) });
const decodeOrgChangesEvent = Schema.decodeUnknownOption(Schema.fromJsonString(orgChangesEventSchema));
const decodeOrganizationEvent = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Struct({ id: Schema.String })));

function buildOrgChangesUrl(organizationSlug: string) {
  const link = linkOptions({ to: "/api/org/changes", search: { organizationSlug } });
  return `${link.to}?${new URLSearchParams(link.search).toString()}`;
}

/** What each change stream name other than a Store family does: an Org Store collection refetches since its cursor, then its declaration invalidates what else it makes stale. */
const invalidations = new Map<ChangeName, Invalidate>([
  ...orgCollections.map((collection): [ChangeName, Invalidate] => [collection.name, (organizationSlug, scope) => {
    void orgStoreTables[collection.name](organizationSlug, scope).utils.refetch();
    collection.invalidate?.(organizationSlug, scope);
  }]),
  ...orgChanges.map((change): [ChangeName, Invalidate] => [change.name, change.invalidate]),
]);

const storeViews: ReadonlySet<ChangeName> = new Set(storeViewNames);
const isStoreView = (name: ChangeName): name is StoreViewName => storeViews.has(name);

export function applyOrganizationChanges(names: readonly ChangeName[], organizationSlug: string, scope: CollectionScope) {
  // One refetch for every Store family named together, so a view two of them back reads once.
  const views = names.filter(isStoreView);
  if (views.length > 0) refetchStoreViews(organizationSlug, scope, views);
  for (const name of names) if (!isStoreView(name)) invalidations.get(name)?.(organizationSlug, scope);
}

/**
 * One change stream per Organization tab. Each named collection refetches only rows changed since its cursor. Every
 * cache of the tab is keyed by slug, so once the slug names another Organization, as after a delete and a new one taking
 * it, the page reloads: nothing the old Organization left in memory survives.
 */
export function useOrganizationChanges(organizationSlug: string, organizationId: string) {
  const { queryClient, sessionId, userId } = useCollectionScope();
  useEffect(
    () => watchOrganizationChanges(organizationSlug, organizationId, { queryClient, sessionId, userId }),
    [organizationSlug, organizationId, queryClient, sessionId, userId],
  );
}

export function watchOrganizationChanges(
  organizationSlug: string,
  organizationId: string,
  scope: CollectionScope,
  replaced: () => void = () => window.location.reload(),
) {
  // The stream never resumes: each connect starts at the server's current horizon. `open` fires on every
  // connect and reconnect, and refetching each collection since its own cursor is the only gap recovery.
  return liveStream(buildOrgChangesUrl(organizationSlug), {
    onOpen: () => applyOrganizationChanges(changeNameSchema.literals, organizationSlug, scope),
    on: {
      organization: (event) => {
        const named = decodeOrganizationEvent(event.data);
        if (Option.isSome(named) && named.value.id !== organizationId) replaced();
      },
      changes: (event) => {
        const changes = decodeOrgChangesEvent(event.data);
        if (Option.isSome(changes)) applyOrganizationChanges(changes.value.collections, organizationSlug, scope);
      },
    },
    silenceMs: 45_000,
  });
}
