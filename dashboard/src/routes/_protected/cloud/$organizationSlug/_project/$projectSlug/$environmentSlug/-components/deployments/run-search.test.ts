import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { deploymentDestination, nodeDestination, originFrom, runSearchSchema } from "./run-search";

const scope = { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" };
const decode = Schema.decodeUnknownSync(runSearchSchema);

describe("run search", () => {
  it("opens a Run in its own Environment, with only that Environment's params", () => {
    const drawer = { ...scope, resourceId: "vol-1" };
    expect(deploymentDestination(drawer, "d1", { job: "api-id" })).toEqual({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments/$deploymentId",
      params: { ...scope, deploymentId: "d1" },
      search: { job: "api-id" },
    });
  });

  it("goes back to the node it was opened from, in that node's Environment", () => {
    const back = (node: Parameters<typeof originFrom>[1]) => nodeDestination("acme", scope, originFrom({ ...scope, environmentSlug: "staging" }, node));
    expect(back({ kind: "service", id: "api-id" })).toMatchObject({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId",
      params: { ...scope, environmentSlug: "staging", serviceId: "api-id" },
    });
    for (const kind of ["volume", "config", "mirror"] as const) {
      expect(back({ kind, id: "r1" })).toMatchObject({
        to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/resources/$resourceId",
        params: { ...scope, environmentSlug: "staging", resourceId: "r1" },
      });
    }
    expect(back({ kind: "live", name: "pr-12" })).toMatchObject({ params: { lineageId: "pr-12" } });
  });

  it("goes back to Architecture without an origin", () => {
    expect(nodeDestination("acme", scope, undefined)).toEqual({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug", params: scope, search: {},
    });
  });

  it("drops an origin or logs stage it can't read, keeping the rest", () => {
    expect(decode({ job: "api-id", origin: { node: { kind: "service" } }, logs: "nope" })).toEqual({ job: "api-id" });
    expect(decode({ logs: "deploy" })).toEqual({ logs: "deploy" });
  });
});
