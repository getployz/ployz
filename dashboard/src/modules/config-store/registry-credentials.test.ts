import { expect, it } from "vitest";
import { getRegistryHostFromImageReference } from "./registry-credentials";

it("reads a registry host only before a slash", () => {
  expect(getRegistryHostFromImageReference("nginx:1.27-alpine")).toBe("docker.io");
  expect(getRegistryHostFromImageReference("postgres:16")).toBe("docker.io");
  expect(getRegistryHostFromImageReference("acme/api:1")).toBe("docker.io");
  expect(getRegistryHostFromImageReference("ghcr.io/acme/api:1")).toBe("ghcr.io");
  expect(getRegistryHostFromImageReference("localhost:5000/api")).toBe("localhost:5000");
});
