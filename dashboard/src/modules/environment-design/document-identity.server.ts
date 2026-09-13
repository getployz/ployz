import "@tanstack/react-start/server-only";
import { getTableName, sql } from "drizzle-orm";
import { environmentResource, service } from "#/modules/environment-design/tables";
import { environment } from "#/modules/project/tables";

const environmentResourceTable = sql.identifier(getTableName(environmentResource));
const environmentResourceId = sql.identifier(environmentResource.id.name);
const environmentResourceEnvironmentId = sql.identifier(environmentResource.environmentId.name);
const serviceTable = sql.identifier(getTableName(service));
const serviceId = sql.identifier(service.id.name);
const serviceEnvironmentId = sql.identifier(service.environmentId.name);

export const volumeIsAuthored = sql<boolean>`exists (
  select 1 from ${environment} document,
  jsonb_array_elements(document.intent->'volumes') node
  where document.id = ${environmentResourceTable}.${environmentResourceEnvironmentId}
    and node->>'resourceId' = ${environmentResourceTable}.${environmentResourceId}::text
)`;
export const serviceIsAuthored = sql<boolean>`exists (
  select 1 from ${environment} document,
  jsonb_array_elements(document.intent->'services') node
  where document.id = ${serviceTable}.${serviceEnvironmentId}
    and node->>'id' = ${serviceTable}.${serviceId}::text
)`;
