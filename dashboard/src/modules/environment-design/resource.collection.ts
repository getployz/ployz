import { BasicIndex, collectionOptions, liveQueryCollectionOptions, type DbClient, eq, toArray, type Collection, type UtilsRecord } from "@tanstack/react-db";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import { volumeDocumentRecord, volumeIsVisible, type VolumeHistory, type ResourceDocumentView } from "./resource-document";

import type { getRawEnvironmentResourcesCollection, getResourceLineagesCollection, getCanvasPositionsCollection, getEnvironmentNodeConfigSnapshotsCollection, getVolumeRemoveAttemptsCollection } from "#/collections/collections";
import type { getEnvironmentDocumentsCollection } from "./environment-document.collection";

type Source<C> = C extends Collection<infer Row, infer Key, infer _Utils, infer Schema, infer Input>
  ? Collection<Row, Key, UtilsRecord, Schema, Input> : never;
type ResourceSources = {
  resources: Source<ReturnType<typeof getRawEnvironmentResourcesCollection>>;
  lineages: Source<ReturnType<typeof getResourceLineagesCollection>>;
  positions: Source<ReturnType<typeof getCanvasPositionsCollection>>;
  documents: Source<ReturnType<typeof getEnvironmentDocumentsCollection>>;
};
type VolumeSources = ResourceSources & {
  snapshots: Source<ReturnType<typeof getEnvironmentNodeConfigSnapshotsCollection>>;
  removals: Source<ReturnType<typeof getVolumeRemoveAttemptsCollection>>;
};

function volumeDocumentRows(client: DbClient, { resources, lineages, positions, documents }: ResourceSources) {
  const resourcePositions = client.collection(collectionOptions({ ...liveQueryCollectionOptions({
    id: `${positions.id}:volume-positions`,
    query: (q) => q.from({ position: positions }).where(({ position }) => eq(position.resourceType, "volume")),
    getKey: (position) => position.resourceId,
  }), autoIndex: "eager", defaultIndexType: BasicIndex }));
  return client.collection(collectionOptions({ ...liveQueryCollectionOptions({
    id: `${resources.id}:volume-document-rows`,
    query: (q) => q.from({ resource: resources })
      .where(({ resource }) => eq(resource.implementationType, "volume"))
      .innerJoin({ lineage: lineages }, ({ resource, lineage }) => eq(resource.lineageId, lineage.id))
      .innerJoin({ document: documents }, ({ resource, document }) => eq(resource.environmentId, document.id))
      .leftJoin({ position: resourcePositions }, ({ resource, position }) => eq(resource.id, position.resourceId))
      .fn.select(({ resource, lineage, document, position }) => ({ resource, lineage, document, position })),
    getKey: (row) => row.resource.id,
  }), autoIndex: "eager", defaultIndexType: BasicIndex }));
}

function documentView(row: ReturnType<typeof volumeDocumentRows> extends { values(): IterableIterator<infer R> } ? R : never) {
  const { organizationId: _resourceOrganization, ...resource } = withoutVirtualProps(row.resource);
  const { organizationId: _lineageOrganization, ...lineage } = withoutVirtualProps(row.lineage);
  const position = row.position;
  let canvasPosition: ResourceDocumentView["canvasPosition"] = null;
  if (position) {
    const { organizationId: _positionOrganization, ...record } = withoutVirtualProps(position);
    canvasPosition = record;
  }
  return { resource, lineage, document: row.document,
    canvasPosition,
    projectSlug: row.document.projectSlug, environmentSlug: row.document.namespace };
}

export function createVolumeResourcesCollection(input: { client: DbClient; sources: VolumeSources }) {
  const client = input.client;
  const resources = volumeDocumentRows(input.client, input.sources);
  const { snapshots, removals } = input.sources;
  const rows = client.collection(collectionOptions(liveQueryCollectionOptions({
    id: `${input.sources.resources.id}:volume-history`,
    query: (q) => q.from({ resource: input.sources.resources })
      .where(({ resource }) => eq(resource.implementationType, "volume"))
      .select(({ resource }) => ({ resourceId: resource.id,
      snapshots: toArray(q.from({ snapshot: snapshots })
        .where(({ snapshot }) => eq(snapshot.nodeType, "volume"))
        .where(({ snapshot }) => eq(snapshot.nodeId, resource.id))
        .fn.where(({ snapshot }) => snapshot.config !== null)
        .orderBy(({ snapshot }) => snapshot.createdAt, "desc").findOne()),
      removals: toArray(q.from({ removal: removals })
        .where(({ removal }) => eq(removal.environmentResourceId, resource.id))
        .where(({ removal }) => eq(removal.status, "completed"))
        .orderBy(({ removal }) => removal.terminalAt, "desc").findOne()),
    })),
  })));
  const withHistory = client.collection(collectionOptions(liveQueryCollectionOptions({
    id: `${input.sources.resources.id}:volume-document-history`,
    query: (q) => q.from({ history: rows }).fn.select(({ history }) => {
      // Correlated arrays can be null while a refreshed parent row is removed.
      const dates = (history.removals ?? []).flatMap((removal) =>
        removal.terminalAt ? [removal.terminalAt] : [],
      );
      return { resourceId: history.resourceId, history: {
        snapshot: history.snapshots?.[0] ?? null,
        removedAt: dates.length ? new Date(Math.max(...dates.map((date) => date.getTime()))) : null,
      } satisfies VolumeHistory };
    }),
  })));
  return client.collection(collectionOptions(liveQueryCollectionOptions({
    id: `${input.sources.resources.id}:volume-resources`,
    query: (q) => q.from({ row: resources })
      .innerJoin({ history: withHistory }, ({ row, history }) => eq(row.resource.id, history.resourceId))
      .fn.where(({ row, history }) => volumeIsVisible(documentView(row), history.history))
      .fn.select(({ row, history }) => {
        const record = volumeDocumentRecord(documentView(row), history.history);
        if (!record) throw new Error("Volume history is not visible.");
        return record;
      }),
    getKey: (item) => item.resource.id,
  })));
}
