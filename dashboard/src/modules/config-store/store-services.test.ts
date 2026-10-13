import { describe, expect, it } from "vitest";
import type { DiffView, DomainRow, EnvironmentView, ProjectListing, RowId, ServiceListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { scalarSetting, serviceSetting, settingChange, settingError } from "./catalog";
import { deployedVcpuLimit, domainChanged, newProjectName, newServiceName, serviceChangeCount, serviceChanges, serviceSettingRows } from "./store-services";

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

const project = (name: string): ProjectListing => ({ id: `${name}-id`, name, default_environment: "production", environments: ["production"] });

describe("newProjectName", () => {
  it("names a Project after its first Service's repository or image, with a random suffix past Project names taken", () => {
    expect(newProjectName({ type: "git", repository: "getployz/Shop.Web", branch: "main" }, [project("api")])).toBe("shop-web");
    expect(newProjectName({ type: "image", image: "ghcr.io/acme/web:1.2" }, [])).toBe("web");
    expect(newProjectName({ type: "git", repository: "getployz/api", branch: null }, [project("api")]))
      .toMatch(/^api-[a-z0-9]{4}$/u);
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
    // Memory takes GB or a size with a unit; its field edits GB.
    const memory = scalarSetting("memLimit");
    expect(settingError(memory, "1.5")).toBeNull();
    expect(settingError(memory, "2000")).toBe("Enter a number above 0, up to 1024.");
    expect(settingChange("web.memLimit", memory, "1.5")).toEqual({ op: "set", path: "web.memLimit", value: 1.5 });
  });
});

it("reads one Service's pink trail from the Environment's diff, a rename under `name`", () => {
  const diff = asTestDouble<DiffView>()({
    changes: [{ type: "service", id: "a", name: "frontend", lifecycle: "update", restarts: [], comparison: "head", settings: [
      { path: "frontend.name", kind: "update", before: "web", after: "frontend", canRestore: true },
      { path: "frontend.replicas", kind: "update", before: 1, after: 3, canRestore: true },
    ] }],
  });
  const changes = serviceChanges(diff, "a");
  expect([...changes.keys()]).toEqual(["name", "replicas"]);
  expect(changes.get("name")?.before).toBe("web");
  expect(serviceChanges(diff, "b").size).toBe(0);
});

it("limits a Service's CPU by its deployed limit and replicas, not what the next Deploy stages", () => {
  const settings = asTestDouble<EnvironmentView>()({ settings: [
    { path: "web.cpuLimit", value: 2, default: null },
    { path: "web.replicas", value: 3, default: 1 },
  ] });
  const staged = (settings: readonly { path: string; kind: "update"; before: number | null; after: number; canRestore: boolean }[]) =>
    serviceChanges(asTestDouble<DiffView>()({ changes: [{ type: "service", id: "a", name: "web", lifecycle: "update", restarts: [], comparison: "head", settings }] }), "a");
  const rows = serviceSettingRows(settings, "web");
  expect(deployedVcpuLimit(rows, staged([]))).toBe(6);
  expect(deployedVcpuLimit(rows, staged([
    { path: "web.cpuLimit", kind: "update", before: 0.5, after: 2, canRestore: true },
    { path: "web.replicas", kind: "update", before: 1, after: 3, canRestore: true },
  ]))).toBe(0.5);
  expect(deployedVcpuLimit(rows, staged([{ path: "web.cpuLimit", kind: "update", before: null, after: 2, canRestore: true }]))).toBeNull();
  expect(deployedVcpuLimit(rows, staged([{ path: "web.replicas", kind: "update", before: 0, after: 3, canRestore: true }]))).toBeNull();
});

it("reads a source change as a change to each field of it that differs", () => {
  const diff = asTestDouble<DiffView>()({
    changes: [{ type: "service", id: "a", name: "web", lifecycle: "update", restarts: [], comparison: "head", settings: [
      { path: "web.source", kind: "update", before: { type: "git", repository: "acme/web", rootDir: "/" },
        after: { type: "image", image: "web:2", credentials: true }, canRestore: true },
    ] }],
  });
  const changes = serviceChanges(diff, "a");
  expect([...changes.keys()]).toEqual(["source", "image", "repository", "rootDir", "registryCredential"]);
  expect(changes.get("image")).toMatchObject({ path: "web.source", before: null, after: "web:2" });
  expect(changes.get("repository")).toMatchObject({ before: "acme/web", after: null });
  expect(changes.get("registryCredential")).toMatchObject({ before: null, after: { secret: true } });
  // One source row is one change on the card, whichever of its fields it moves.
  expect(serviceChangeCount(diff, "a")).toBe(1);
});

it("marks the domains the next Deploy changes: the generated one by its list, a custom one by its route's hostname", () => {
  const route = (hostname: string, targetPort: number | null) => ({ id: `${hostname}-id`, hostname, targetPort });
  const diff = asTestDouble<DiffView>()({
    changes: [{ type: "service", id: "a", name: "web", lifecycle: "update", restarts: [], comparison: "head", settings: [
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
  // A second generated domain marks only itself.
  const another = { ...listChanged, before: [{ prefix: "web", targetPort: null }], after: [{ prefix: "web", targetPort: null }, { prefix: "shop", targetPort: null }] };
  expect(domainChanged(new Map([["managedHostnames", another]]), domain({ kind: "generated", prefix: "web", hostname: null }))).toBe(false);
  expect(domainChanged(new Map([["managedHostnames", another]]), domain({ kind: "generated", prefix: "shop", hostname: null }))).toBe(true);
});
