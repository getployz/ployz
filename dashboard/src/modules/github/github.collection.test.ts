// @vitest-environment jsdom
import { expect, it } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { createGithubReposCollection, getRawGithubReposCollection } from "./github.collection";
import { createApiCollection } from "#/collections/query-collection";

it("updates the selector projection and isolates request and authenticated session caches", async () => {
  const client = new QueryClient();
  const otherClient = new QueryClient();
  const scope = { queryClient: client, userId: "user", sessionId: "first" };
  const row = {
    userId: "user", installationId: 12, repositoryId: 42, name: "repo", fullName: "acme/repo",
    defaultBranch: "main", private: true, htmlUrl: "https://github.com/acme/repo",
    repoUpdatedAt: new Date("2026-01-01T00:00:00Z"), syncedAt: new Date("2026-01-02T00:00:00Z"),
  };
  let rows = [row];
  const raw = createApiCollection({ queryClient: client, queryKey: ["test"], queryFn: async () => rows, getKey: (row: typeof rows[number]) => row.repositoryId });
  await raw.preload();
  const view = createGithubReposCollection(raw, "test-view");
  const subscription = view.subscribeChanges(() => {});
  await view.preload();
  expect(Array.from(view.values())).toMatchObject([{ id: 42, full_name: "acme/repo", repo_updated_at: "2026-01-01T00:00:00.000Z" }]);
  rows = [{ ...row, name: "renamed", fullName: "acme/renamed" }];
  await raw.utils.refetch();
  expect(Array.from(view.values())[0]?.full_name).toBe("acme/renamed");
  rows = [];
  await raw.utils.refetch();
  expect(view.size).toBe(0);
  const scoped = getRawGithubReposCollection(scope);
  expect(getRawGithubReposCollection(scope)).toBe(scoped);
  expect(getRawGithubReposCollection({ ...scope, queryClient: otherClient })).not.toBe(scoped);
  expect(getRawGithubReposCollection({ ...scope, sessionId: "second" })).not.toBe(scoped);
  subscription.unsubscribe();
  await view.cleanup();
  await raw.cleanup();
  client.clear();
  otherClient.clear();
});
