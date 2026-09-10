import type { CollectionScope } from "#/collections/scope";
import { getEnvironmentsCollection, getRawServicesCollection, getRawEnvironmentResourcesCollection,
  getResourceLineagesCollection, getCanvasPositionsCollection, getEnvironmentNodeIntroductionsCollection } from "#/collections/collections";
import type { createServiceServerFn } from "./service-functions";
import type { createVolumeResourceServerFn } from "./resource-functions";

export async function applyCreatedService(organizationSlug: string, scope: CollectionScope,
  result: Awaited<ReturnType<typeof createServiceServerFn>>["data"]) {
  await Promise.all([
    getEnvironmentsCollection(organizationSlug, scope).writeCommitted(result.environment),
    getRawServicesCollection(organizationSlug, scope).writeCommitted(result.identity),
    getCanvasPositionsCollection(organizationSlug, scope).writeCommitted(result.canvasPosition),
    getEnvironmentNodeIntroductionsCollection(organizationSlug, scope).writeCommitted(result.introduction),
  ]);
}

export async function applyCreatedResource(organizationSlug: string, scope: CollectionScope,
  result: Omit<Awaited<ReturnType<typeof createVolumeResourceServerFn>>, "data">) {
  await Promise.all([
    getEnvironmentsCollection(organizationSlug, scope).writeCommitted(result.environment),
    getRawEnvironmentResourcesCollection(organizationSlug, scope).writeCommitted(result.resource),
    getResourceLineagesCollection(organizationSlug, scope).writeCommitted(result.lineage),
    getCanvasPositionsCollection(organizationSlug, scope).writeCommitted(result.canvasPosition),
    getEnvironmentNodeIntroductionsCollection(organizationSlug, scope).writeCommitted(result.introduction),
  ]);
}
