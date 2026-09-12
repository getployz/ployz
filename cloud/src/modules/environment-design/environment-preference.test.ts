import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import type { selectEnvironmentServerFn } from "./workspace-functions";
import { environmentBySlugQueryOptions, environmentKeys, projectListQueryOptions, rememberSelectedEnvironment } from "./workspace-queries";

it("remembers cached navigation and serializes rapid environment selections", async () => {
  const client = new QueryClient();
  const input = { organizationSlug: "acme", projectSlug: "api" };
  const production = { id: "prod", projectId: "project", organizationId: "org", name: "Production", namespace: "production" };
  const staging = { ...production, id: "stage", name: "Staging", namespace: "staging" };
  for (const environment of [production, staging]) {
    client.setQueryData(environmentBySlugQueryOptions("acme", "api", environment.namespace).queryKey, environment);
  }
  const projectsKey = projectListQueryOptions("acme").queryKey;
  client.setQueryData(projectsKey, [{ id: "project", organizationId: "org", name: "API", slug: "api", firstEnvironment: production, userDefaultEnvironmentId: null, resolvedEnvironment: production }]);
  let resolveFirst: (value: typeof staging) => void = () => {};
  const first = { promise: new Promise<typeof staging>((resolve) => { resolveFirst = resolve; }), resolve: (value: typeof staging) => resolveFirst(value) };
  const selectEnvironment = vi.fn<typeof selectEnvironmentServerFn>().mockImplementationOnce(() => first.promise).mockResolvedValueOnce(production);
  const stageWrite = rememberSelectedEnvironment(client, { ...input, environmentSlug: "staging" }, selectEnvironment);
  await vi.waitFor(() => expect(selectEnvironment).toHaveBeenCalledTimes(1));
  const prodWrite = rememberSelectedEnvironment(client, { ...input, environmentSlug: "production" }, selectEnvironment);
  await vi.waitFor(() => expect(client.getQueryData(environmentKeys.preferred("acme", "api"))).toEqual(production));
  expect(selectEnvironment).toHaveBeenCalledTimes(1);
  first.resolve(staging);
  await Promise.all([stageWrite, prodWrite]);
  expect(selectEnvironment).toHaveBeenLastCalledWith({ data: { ...input, environmentSlug: "production" } });
  expect(client.getQueryData<Array<{ resolvedEnvironment: typeof production }>>(projectsKey)?.[0]?.resolvedEnvironment).toEqual(production);
  client.clear();
});
