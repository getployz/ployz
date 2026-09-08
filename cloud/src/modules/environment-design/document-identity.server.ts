import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { environment } from "#/modules/project/tables";
import { environmentResource, service } from "./tables";

export const volumeIsAuthored = sql<boolean>`exists (
  select 1 from ${environment} document,
  jsonb_array_elements(document.intent->'volumes') node
  where document.id = ${environmentResource.environmentId}
    and node->>'resourceId' = ${environmentResource.id}::text
)`;
export const serviceIsAuthored = sql<boolean>`exists (
  select 1 from ${environment} document,
  jsonb_array_elements(document.intent->'services') node
  where document.id = ${service.environmentId}
    and node->>'id' = ${service.id}::text
)`;
