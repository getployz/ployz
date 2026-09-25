import { linkOptions } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { useEffect } from "react";
import * as EffectRecord from "effect/Record";
import { orgStoreTables } from "./collections";
import { changeNameSchema, type ChangeName } from "./read.contract";
import type { CollectionScope } from "./scope";
import { useCollectionScope } from "./use-collection-scope";
import { organizationKeys } from "#/modules/environment-design/workspace.queries";

const orgChangesEventSchema = Schema.Struct({ collections: Schema.Array(changeNameSchema) });
const decodeOrgChangesEvent = Schema.decodeUnknownOption(Schema.fromJsonString(orgChangesEventSchema));

function buildOrgChangesUrl(organizationSlug: string) {
  const link = linkOptions({ to: "/api/org/changes", search: { organizationSlug } });
  return `${link.to}?${new URLSearchParams(link.search).toString()}`;
}

type Refetch = (organizationSlug: string, scope: CollectionScope) => void;

/** What each change stream name refetches: its collection since its cursor, or the organization state (its name). */
const refetches = {
  ...EffectRecord.map(orgStoreTables, (get): Refetch => (organizationSlug, scope) => void get(organizationSlug, scope).utils.refetch()),
  organization: (_organizationSlug: string, scope: CollectionScope) => void scope.queryClient.invalidateQueries({ queryKey: organizationKeys.all }),
} satisfies Record<ChangeName, Refetch>;

export function applyOrganizationChanges(names: readonly ChangeName[], organizationSlug: string, scope: CollectionScope) {
  for (const name of names) refetches[name](organizationSlug, scope);
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
  const source = new EventSource(buildOrgChangesUrl(organizationSlug));
  const refetchAll = () => applyOrganizationChanges(EffectRecord.keys(refetches), organizationSlug, scope);
  const handleChanges = (event: MessageEvent<string>) => {
    const changes = decodeOrgChangesEvent(event.data);
    if (Option.isSome(changes)) applyOrganizationChanges(changes.value.collections, organizationSlug, scope);
  };
  // The stream never resumes: each connect starts at the server's current horizon. `open` fires on every
  // connect and reconnect, and refetching each collection since its own cursor is the only gap recovery.
  source.addEventListener("open", refetchAll);
  source.addEventListener("changes", handleChanges);
  return () => source.close();
}
