import { linkOptions } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { useEffect } from "react";
import * as EffectRecord from "effect/Record";
import { orgStoreTables } from "./collections";
import { changeNameSchema, storeViewNames, type ChangeName, type StoreViewName } from "./read.contract";
import type { CollectionScope } from "./scope";
import { useCollectionScope } from "./use-collection-scope";
import { organizationKeys } from "#/modules/organization/organization-state.queries";
import { refetchStoreViews, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { serverDrainKeys } from "#/modules/machines/server-drain.queries";
import { serverWebTrafficKeys } from "#/modules/machines/server-web-traffic.queries";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { serviceActKeys } from "#/modules/service-act/service-act.queries";
import { volumePlacementKeys } from "#/modules/volume-placement/volume-placement.queries";
import { serverUpgradeKeys } from "#/modules/server-upgrade/server-upgrade.queries";
import { membersKey } from "#/modules/organization/members.queries";
import { liveStream } from "#/lib/live.stream";

const orgChangesEventSchema = Schema.Struct({ collections: Schema.Array(changeNameSchema) });
const decodeOrgChangesEvent = Schema.decodeUnknownOption(Schema.fromJsonString(orgChangesEventSchema));

function buildOrgChangesUrl(organizationSlug: string) {
  const link = linkOptions({ to: "/api/org/changes", search: { organizationSlug } });
  return `${link.to}?${new URLSearchParams(link.search).toString()}`;
}

type Refetch = (organizationSlug: string, scope: CollectionScope) => void;

/** What each change stream name other than a Store view refetches: its collection since its cursor, or the organization state. */
const refetches = {
  ...EffectRecord.map(orgStoreTables, (get): Refetch => (organizationSlug, scope) => void get(organizationSlug, scope).utils.refetch()),
  // What support may read changed: every Store view rereads, so support's cache drops what it may no longer see.
  organization_support_access: (organizationSlug: string, scope: CollectionScope) => {
    void orgStoreTables.organization_support_access(organizationSlug, scope).utils.refetch();
    void scope.queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug) }, { cancelRefetch: false });
  },
  organization: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: organizationKeys.all }),
  server_upgrade: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: serverUpgradeKeys.all }),
  server_drain: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: serverDrainKeys.all }),
  server_web_traffic: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: serverWebTrafficKeys.all }),
  volume_run: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: volumeRunKeys.all }),
  service_act: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: serviceActKeys.all }),
  volume_placement: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all }),
  members: (organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: membersKey(organizationSlug) }),
} satisfies Record<Exclude<ChangeName, StoreViewName>, Refetch>;

const storeViews: ReadonlySet<ChangeName> = new Set(storeViewNames);
const isStoreView = (name: ChangeName): name is StoreViewName => storeViews.has(name);

export function applyOrganizationChanges(names: readonly ChangeName[], organizationSlug: string, scope: CollectionScope) {
  // One refetch for every Store family named together, so a view two of them back reads once.
  const views = names.filter(isStoreView);
  if (views.length > 0) refetchStoreViews(organizationSlug, scope, views);
  for (const name of names) if (!isStoreView(name)) refetches[name](organizationSlug, scope);
}

/** One change stream per Organization tab. Each named collection refetches only rows changed since its cursor. */
export function useOrganizationChanges(organizationSlug: string) {
  const { queryClient, sessionId, userId } = useCollectionScope();
  useEffect(
    () => watchOrganizationChanges(organizationSlug, { queryClient, sessionId, userId }),
    [organizationSlug, queryClient, sessionId, userId],
  );
}

export function watchOrganizationChanges(organizationSlug: string, scope: CollectionScope) {
  // The stream never resumes: each connect starts at the server's current horizon. `open` fires on every
  // connect and reconnect, and refetching each collection since its own cursor is the only gap recovery.
  return liveStream(buildOrgChangesUrl(organizationSlug), {
    onOpen: () => applyOrganizationChanges([...EffectRecord.keys(refetches), ...storeViewNames], organizationSlug, scope),
    on: {
      changes: (event) => {
        const changes = decodeOrgChangesEvent(event.data);
        if (Option.isSome(changes)) applyOrganizationChanges(changes.value.collections, organizationSlug, scope);
      },
    },
    silenceMs: 45_000,
  });
}
