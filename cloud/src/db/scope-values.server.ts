import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";

export function organizationIdForProject(projectId: string) {
  return sql<string>`(select organization_id from project where id = ${projectId})`;
}

export function organizationIdForEnvironment(environmentId: string) {
  return sql<string>`(select organization_id from environment where id = ${environmentId})`;
}

export function organizationIdForService(serviceId: string) {
  return sql<string>`(select organization_id from service where id = ${serviceId})`;
}

export function organizationIdForDeployment(environmentDeploymentId: string) {
  return sql<string>`(select organization_id from environment_deployment where id = ${environmentDeploymentId})`;
}
