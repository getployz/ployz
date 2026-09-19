import { expect, test } from "vitest";
import { imageRegistryLink, isValidImageReference } from "./image-registry-link";

test("links image repositories without treating tags or digests as registry hosts", () => {
  expect(imageRegistryLink("nginx")).toBe("https://hub.docker.com/_/nginx");
  expect(imageRegistryLink("nginx:latest")).toBe("https://hub.docker.com/_/nginx");
  expect(imageRegistryLink(`docker.io/library/nginx@sha256:${"a".repeat(64)}`)).toBe("https://hub.docker.com/_/nginx");
  expect(imageRegistryLink("acme/api:main")).toBe("https://hub.docker.com/r/acme/api");
  expect(imageRegistryLink("quay.io/acme/api:main")).toBe("https://quay.io/repository/acme/api");
  expect(imageRegistryLink("registry.example.com:5000/acme/api:main")).toBe("https://registry.example.com:5000/acme/api");
  expect(imageRegistryLink("https://example.com/image")).toBeNull();
  expect(imageRegistryLink("registry.example.com/../image")).toBeNull();
  expect(imageRegistryLink("")).toBeNull();
});

test("rejects incomplete references while allowing tags, registries and digests", () => {
  for (const invalid of ["nginx:", "nginx@", "nginx@sha256:abc", "acme/", "bad image", "https://nginx", "Nginx", "nginx:bad:tag"]) {
    expect(isValidImageReference(invalid), invalid).toBe(false);
    expect(imageRegistryLink(invalid), invalid).toBeNull();
  }
  for (const valid of ["nginx", "nginx:latest", "nginx:RC1", "registry.example.com:5000/acme/api:v1", "[::1]:5000/api", `nginx@sha256:${"a".repeat(64)}`]) {
    expect(isValidImageReference(valid), valid).toBe(true);
  }
});
