import { expect, it } from "vitest";
import { resolveDefaultEnvironment } from "./workspace.queries";

const environment = (id: string, projectId: string, createdAt: number) => ({ id, projectId, createdAt: new Date(createdAt) });
// Another project's older Environment never resolves for this one.
const environments = [environment("staging", "api", 2), environment("other", "web", 0), environment("production", "api", 1)];

it("opens the Default Environment", () => {
  expect(resolveDefaultEnvironment({ id: "api", defaultEnvironmentId: "staging" }, environments)?.id).toBe("staging");
});

it("opens the oldest Environment when the Default Environment is unset or gone", () => {
  expect(resolveDefaultEnvironment({ id: "api", defaultEnvironmentId: null }, environments)?.id).toBe("production");
  expect(resolveDefaultEnvironment({ id: "api", defaultEnvironmentId: "other" }, environments)?.id).toBe("production");
});

it("opens nothing for a project without Environments", () => {
  expect(resolveDefaultEnvironment({ id: "empty", defaultEnvironmentId: null }, environments)).toBeNull();
});
