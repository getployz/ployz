// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { expect, it } from "vitest";
import { getEnvironmentsCollection, getProjectsCollection } from "#/collections/collections";
import { createApiCollection, reconcileCollection } from "#/collections/query-collection";
import { createEnvironmentDocumentsCollection, getEnvironmentDocumentsCollection } from "./environment-document.collection";
import type { project as projectTable } from "#/modules/project/tables";
import type { EnvironmentDocument } from "./working-state-repository.server";


it("reconciles an unobserved collection, updates joined documents, and isolates authenticated scopes", async () => {
  const client = new QueryClient();
  const otherClient = new QueryClient();
  const scope = { queryClient: client, sessionId: "session", userId: "user" };
  const project: typeof projectTable.$inferSelect = { id: crypto.randomUUID(), organizationId: crypto.randomUUID(), name: "Project", slug: "project", createdAt: new Date() };
  const environment: EnvironmentDocument = { id: crypto.randomUUID(), projectId: project.id, organizationId: project.organizationId,
    name: "Production", namespace: "production", revision: crypto.randomUUID(), createdAt: new Date(), updatedAt: new Date(),
    intent: { version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [] } };
  let projects = [project];
  let environments = [environment];
  const raw = createApiCollection({ queryClient: client, queryKey: ["test", "environments"],
    queryFn: async () => environments, getKey: (row: EnvironmentDocument) => row.id });
  await reconcileCollection(raw);
  expect(raw.get(environment.id)?.revision).toBe(environment.revision);
  const rawProjects = createApiCollection({ queryClient: client, queryKey: ["test", "projects"],
    queryFn: async () => projects, getKey: (row: typeof project) => row.id });
  await rawProjects.preload();
  const documents = createEnvironmentDocumentsCollection("acme", { environments: raw, projects: rawProjects });
  const subscription = documents.subscribeChanges(() => {});
  await documents.preload();
  expect(documents.get(environment.id)?.projectSlug).toBe("project");
  projects = [{ ...project, slug: "renamed" }];
  environments = [{ ...environment, revision: crypto.randomUUID() }];
  await Promise.all([reconcileCollection(rawProjects), reconcileCollection(raw)]);
  expect(documents.get(environment.id)).toMatchObject({ projectSlug: "renamed", revision: environments[0]?.revision });
  const scopedRaw = getEnvironmentsCollection("acme", scope);
  const scopedDocuments = getEnvironmentDocumentsCollection("acme", scope);
  const scopedProjects = getProjectsCollection("acme", scope);
  expect(getEnvironmentsCollection("acme", scope)).toBe(scopedRaw);
  expect(getEnvironmentDocumentsCollection("acme", scope)).toBe(scopedDocuments);
  for (const other of [{ ...scope, sessionId: "new" }, { ...scope, userId: "other" }, { ...scope, queryClient: otherClient }]) {
    expect(getEnvironmentsCollection("acme", other)).not.toBe(scopedRaw);
    expect(getEnvironmentDocumentsCollection("acme", other)).not.toBe(scopedDocuments);
    expect(getProjectsCollection("acme", other)).not.toBe(scopedProjects);
  }
  expect(getEnvironmentsCollection("other", scope)).not.toBe(scopedRaw);
  environments = [];
  await reconcileCollection(raw);
  expect(documents.size).toBe(0);
  subscription.unsubscribe();
  await documents.cleanup();
  await raw.cleanup();
  await rawProjects.cleanup();
  client.clear();
  otherClient.clear();
});
