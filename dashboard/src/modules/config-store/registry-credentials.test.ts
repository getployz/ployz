import { expect, it } from "vitest";
import { getRegistryHostFromImageReference, registryHostProblem } from "./registry-credentials";

it("reads a registry host only before a slash", () => {
  expect(getRegistryHostFromImageReference("nginx:1.27-alpine")).toBe("docker.io");
  expect(getRegistryHostFromImageReference("postgres:16")).toBe("docker.io");
  expect(getRegistryHostFromImageReference("acme/api:1")).toBe("docker.io");
  expect(getRegistryHostFromImageReference("ghcr.io/acme/api:1")).toBe("ghcr.io");
  expect(getRegistryHostFromImageReference("localhost:5000/api")).toBe("localhost:5000");
});

it("never seals for a lowercased copy of a registry Docker reads with its capitals", () => {
  expect(getRegistryHostFromImageReference("Registry.example.com:5000/acme/web")).not.toBe("registry.example.com:5000");
  expect(registryHostProblem("Registry.example.com:5000/acme/web")).toBe("Write the image's registry in lowercase, like ghcr.io.");
  expect(registryHostProblem("Acme/web:1")).not.toBeNull();
  expect(registryHostProblem("ghcr.io/acme/web:1")).toBeNull();
  expect(registryHostProblem("nginx:1.27")).toBeNull();
});
