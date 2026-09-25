import { orgStoreSeed } from "#/test/org-store-tables";
import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import type { selectEnvironmentServerFn } from "./workspace-functions";
import { preloadWorkspace, rememberSelectedEnvironment } from "./workspace.queries";

it("serializes selections, commits only successful saves, and retries failures", async () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const input = { organizationSlug: "acme", projectSlug: "api" };
  const production = { id: "prod", projectId: "project", organizationId: "org", name: "Production", namespace: "production", createdAt: new Date(0) };
  const staging = { ...production, id: "stage", name: "Staging", namespace: "staging" };
  // Another project's environment shares the namespace; selection must stay within the routed project.
  const otherStaging = { ...staging, id: "other-stage", projectId: "other-project" };
  const projects = [{ id: "other-project", organizationId: "org", name: "Web", slug: "web" }, { id: "project", organizationId: "org", name: "API", slug: "api" }];
  for (const [table, data] of Object.entries({ project: projects, environment_summary: [otherStaging, production, staging], project_preference: [{ id: "project", environmentId: "prod" }] })) {
    queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed<object>(data));
  }
  const collections = await preloadWorkspace("acme", scope);
  try {
    let release = (_value: typeof staging) => {};
    const pending = new Promise<typeof staging>((resolve) => { release = resolve; });
    const select = vi.fn<typeof selectEnvironmentServerFn>().mockImplementationOnce(() => pending).mockResolvedValueOnce(production);
    const first = rememberSelectedEnvironment(scope, { ...input, environmentSlug: "staging" }, select);
    await vi.waitFor(() => expect(select).toHaveBeenCalledTimes(1));
    const second = rememberSelectedEnvironment(scope, { ...input, environmentSlug: "production" }, select);
    expect(collections.preferences.get("project")?.environmentId).toBe("prod");
    release(staging);
    await Promise.all([first, second]);
    expect(select).toHaveBeenCalledTimes(2);
    expect(collections.preferences.get("project")?.environmentId).toBe("prod");
    select.mockRejectedValueOnce(new Error("Offline")).mockResolvedValueOnce(staging);
    await rememberSelectedEnvironment(scope, { ...input, environmentSlug: "staging" }, select);
    expect(collections.preferences.get("project")?.environmentId).toBe("prod");
    await rememberSelectedEnvironment(scope, { ...input, environmentSlug: "staging" }, select);
    expect(select).toHaveBeenCalledTimes(4);
    expect(collections.preferences.get("project")?.environmentId).toBe("stage");
  } finally {
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  }
});
