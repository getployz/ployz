import { BasicIndex, collectionOptions, liveQueryCollectionOptions, type DbClient, eq, type Collection, type UtilsRecord } from "@tanstack/react-db";
import { withoutVirtualProps } from "#/lib/tanstack-db";
import { volumeDocumentRecord, volumeIsVisible, type ResourceDocumentView } from "./resource-document";

import type { getRawEnvironmentResourcesCollection, getResourceLineagesCollection, getCanvasPositionsCollection } from "#/collections/collections";
import type { getEnvironmentDocumentsCollection } from "./environment-document.collection";

type Source<C> = C extends Collection<infer Row, infer Key, infer _Utils, infer Schema, infer Input>
  ? Collection<Row, Key, UtilsRecord, Schema, Input> : never;
type ResourceSources = {
  resources: Source<ReturnType<typeof getRawEnvironmentResourcesCollection>>;
  lineages: Source<ReturnType<typeof getResourceLineagesCollection>>;
  positions: Source<ReturnType<typeof getCanvasPositionsCollection>>;
  documents: Source<ReturnType<typeof getEnvironmentDocumentsCollection>>;
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

export function createVolumeResourcesCollection(input: { client: DbClient; sources: ResourceSources }) {
  const resources = volumeDocumentRows(input.client, input.sources);
  return input.client.collection(collectionOptions(liveQueryCollectionOptions({
    id: `${input.sources.resources.id}:volume-resources`,
    query: (q) => q.from({ row: resources })
      .fn.where(({ row }) => volumeIsVisible(documentView(row)))
      .fn.select(({ row }) => {
        const record = volumeDocumentRecord(documentView(row));
        if (!record) throw new Error("Volume is not visible.");
        return record;
      }),
    getKey: (item) => item.resource.id,
  })));
}
