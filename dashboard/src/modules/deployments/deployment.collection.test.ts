import { QueryClient } from "@tanstack/react-query";
import { expect, it } from "vitest";
import { orgStoreSeed } from "#/test/org-store-tables";
import { activeBuildTailReads } from "./deployment.collection";

it("reads the build tails of the environment's active attempts that build images", async () => {
  const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
  const attempt = (id: string, status: string, needsBuild: boolean, environmentId = "env-1") =>
    ({ id, status, environmentId, targetNodes: { version: 1, nodes: [{ nodeId: "api", needsBuild }] } });
  scope.queryClient.setQueryData(["collections", "session", "user", "org", "environment_deployment"], orgStoreSeed([
    attempt("building", "queued", true), attempt("prebuilt", "deploying", false), attempt("done", "applied", true),
    attempt("elsewhere", "deploying", true, "env-2"),
  ]));
  const reads = await activeBuildTailReads("org", "env-1", scope);
  expect(reads.map((read) => read.queryKey)).toEqual([["deployment-build-tail", "org", "building"]]);
});
