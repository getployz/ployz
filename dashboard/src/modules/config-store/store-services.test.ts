import { describe, expect, it } from "vitest";
import type { DiffView, DomainRow, RowId, ServiceListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { serviceSetting, settingChange, settingError } from "./catalog";
import { domainChanged, newServiceName, serviceChanges } from "./store-services";

const listed = (name: string, privateDns = name): ServiceListing =>
  ({ id: `${name}-id`, row: `${name}:node` as RowId, name, private_dns: privateDns, source: "image", change: null, template: null });

describe("newServiceName", () => {
  it("names a Service from its image or repository, with a random suffix past names and Private DNS already taken", () => {
    expect(newServiceName({ type: "image", image: "ghcr.io/acme/API_Server:1.4" }, [])).toBe("api-server");
    expect(newServiceName({ type: "git", repository: "acme/Shop.Web", branch: null }, [])).toBe("shop-web");
    // `frontend` was `web` once: its Private DNS keeps the name taken.
    expect(newServiceName({ type: "image", image: "web" }, [listed("frontend", "web")])).toMatch(/^web-[a-z0-9]{4}$/u);
    expect(newServiceName({ type: "image", image: `${"a".repeat(70)}:1` }, [listed("a".repeat(63))]))
      .toMatch(new RegExp(`^${"a".repeat(58)}-[a-z0-9]{4}$`, "u"));
    expect(newServiceName({ type: "empty" }, [])).toMatch(/^[a-z]+-[a-z]+$/u);
  });
});

describe("catalog fields", () => {
  it("checks text against the catalog's bounds and turns it into the Store's change", () => {
    const replicas = serviceSetting("replicas");
    expect(settingError(replicas, "3")).toBeNull();
    expect(settingError(replicas, "1.5")).toBe("Enter a whole number from 1 to 50.");
    expect(settingError(replicas, "0")).toBe("Enter a whole number from 1 to 50.");
    expect(settingError(serviceSetting("cpuLimit"), "0")).toBe("Enter a number above 0, up to 64.");
    expect(settingError(serviceSetting("rootDir"), "app")).toBe("That isn't a valid root directory.");
    expect(settingError(replicas, "")).toBeNull();
    for (const field of ["startCommand", "preDeployCommand", "buildCommand", "dockerfilePath", "image", "branch"] as const) {
      expect(settingError(serviceSetting(field), "private\u0000value")).not.toBeNull();
    }
    expect(settingError(serviceSetting("startCommand"), "echo café\nprintf ok")).toBeNull();
    const unicodeCommand = `echo ${"💾".repeat(1995)}`;
    expect(settingError(serviceSetting("startCommand"), unicodeCommand)).toBeNull();
    expect(settingError(serviceSetting("startCommand"), `${unicodeCommand}💾`)).toBe("Use at most 2000 characters.");
    expect(settingError(serviceSetting("startCommand"), "a".repeat(10000))).toBe("Use at most 2000 characters.");
    const healthcheckPath = { title: "Healthcheck path", description: serviceSetting("healthcheck").description, ...serviceSetting("healthcheck").properties.path };
    for (const path of ["/ready\u0001probe", "/ready\tprobe", "/ready\nprobe", "/ready\u0085probe"]) {
      expect(settingError(healthcheckPath, path)).not.toBeNull();
    }
    expect(settingError(healthcheckPath, "/café?escaped=%0A")).toBeNull();
    expect(settingChange("web.replicas", replicas, "3")).toEqual({ op: "set", path: "web.replicas", value: 3 });
    expect(settingChange("web.startCommand", serviceSetting("startCommand"), "npm start")).toEqual({ op: "set", path: "web.startCommand", value: "npm start" });
    expect(settingChange("web.replicas", replicas, "")).toEqual({ op: "unset", path: "web.replicas" });
  });
});

it("reads one Service's pink trail from the Environment's diff, a rename under `name`", () => {
  const diff = asTestDouble<DiffView>()({
    changes: [{ type: "service", id: "a", name: "frontend", lifecycle: "update", comparison: "head", settings: [
      { path: "frontend.name", kind: "update", before: "web", after: "frontend", canRestore: true },
      { path: "frontend.replicas", kind: "update", before: 1, after: 3, canRestore: true },
    ] }],
  });
  const changes = serviceChanges(diff, "a");
  expect([...changes.keys()]).toEqual(["name", "replicas"]);
  expect(changes.get("name")?.before).toBe("web");
  expect(serviceChanges(diff, "b").size).toBe(0);
});

it("marks the domains the next Deploy changes: the generated one by its list, a custom one by its route's hostname", () => {
  const route = (hostname: string, targetPort: number | null) => ({ id: `${hostname}-id`, hostname, targetPort });
  const diff = asTestDouble<DiffView>()({
    changes: [{ type: "service", id: "a", name: "web", lifecycle: "update", comparison: "head", settings: [
      { path: "web.routes.old-id", kind: "delete", before: route("old.acme.com", null), after: null, canRestore: true },
      { path: "web.routes.api-id", kind: "update", before: route("api.acme.com", 3000), after: route("api.acme.com", 8080), canRestore: true },
    ] }],
  });
  const domain = (fields: Partial<DomainRow>) =>
    asTestDouble<DomainRow>()({ service: "web", port: null, status: "ready", reason: null, action: null, ...fields });
  const changes = serviceChanges(diff, "a");
  expect(domainChanged(changes, domain({ kind: "custom", hostname: "api.acme.com" }))).toBe(true);
  expect(domainChanged(changes, domain({ kind: "custom", hostname: "old.acme.com" }))).toBe(true);
  expect(domainChanged(changes, domain({ kind: "custom", hostname: "www.acme.com" }))).toBe(false);
  expect(domainChanged(changes, domain({ kind: "generated", prefix: "web", hostname: null }))).toBe(false);
  const listChanged = { path: "web.managedHostnames", kind: "update" as const, before: [], after: [{ prefix: "web", targetPort: null }], canRestore: true, row: null };
  expect(domainChanged(new Map([["managedHostnames", listChanged]]), domain({ kind: "generated", prefix: "web", hostname: null }))).toBe(true);
});
