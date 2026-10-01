// @vitest-environment jsdom
import { expect, it } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { getGithubReposCollection, githubReposQueryKey, type GithubRepositoryView } from "./github.collection";

it("updates rows and isolates request and authenticated session caches", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const otherClient = new QueryClient();
  const scope = { queryClient: client, userId: "user", sessionId: "first" };
  const row: GithubRepositoryView = {
    id: 42, installation_id: 12, name: "repo", full_name: "acme/repo", default_branch: "main", private: true,
    html_url: "https://github.com/acme/repo", repo_updated_at: "2026-01-01T00:00:00.000Z",
    user_id: "user", synced_at: "2026-01-02T00:00:00.000Z",
  };
  const read = (rows: GithubRepositoryView[]) => client.fetchQuery({ queryKey: githubReposQueryKey(scope), queryFn: async () => rows, staleTime: 0 });
  await read([row]);
  const repos = getGithubReposCollection(scope);
  const subscription = repos.subscribeChanges(() => {});
  await repos.preload();
  expect(Array.from(repos.values())).toMatchObject([row]);
  await read([{ ...row, name: "renamed", full_name: "acme/renamed" }]);
  await expect.poll(() => Array.from(repos.values())[0]?.full_name).toBe("acme/renamed");
  await read([]);
  await expect.poll(() => repos.size).toBe(0);
  expect(getGithubReposCollection(scope)).toBe(repos);
  expect(getGithubReposCollection({ ...scope, queryClient: otherClient })).not.toBe(repos);
  expect(getGithubReposCollection({ ...scope, sessionId: "second" })).not.toBe(repos);
  subscription.unsubscribe();
  await repos.cleanup();
  client.clear();
  otherClient.clear();
});
