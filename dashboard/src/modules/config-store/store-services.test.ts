import { describe, expect, it } from "vitest";
import type { DiffView, ServiceId, ServiceListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { serviceSetting, settingChange, settingError } from "./catalog";
import { newServiceName, serviceChanges } from "./store-services";

const listed = (name: string, privateDns = name): ServiceListing =>
  ({ id: `${name}-id` as ServiceId, name, private_dns: privateDns, source: "image", change: null });

describe("newServiceName", () => {
  it("names a Service from its image or repository, numbered past names and Private DNS already taken", () => {
    expect(newServiceName({ type: "image", image: "ghcr.io/acme/API_Server:1.4" }, [])).toBe("api-server");
    expect(newServiceName({ type: "git", repository: "acme/Shop.Web", branch: null }, [])).toBe("shop-web");
    // `frontend` was `web` once: its Private DNS keeps the name taken.
    expect(newServiceName({ type: "image", image: "web" }, [listed("frontend", "web"), listed("web-2")])).toBe("web-3");
    expect(newServiceName({ type: "image", image: `${"a".repeat(70)}:1` }, [listed("a".repeat(63))])).toBe(`${"a".repeat(61)}-2`);
    expect(newServiceName({ type: "empty" }, [])).toMatch(/^[a-z]+-[a-z]+$/u);
  });
});

describe("catalog fields", () => {
  it("checks text against the catalog's bounds and turns it into the Store's change", () => {
    const replicas = serviceSetting("replicas");
    expect(settingError(replicas, "3")).toBeNull();
    expect(settingError(replicas, "1.5")).toBe("Enter a whole number from 0 to 50.");
    expect(settingError(serviceSetting("cpuLimit"), "0")).toBe("Enter a number above 0, up to 64.");
    expect(settingError(serviceSetting("rootDir"), "app")).toBe("That isn't a valid root directory.");
    expect(settingError(replicas, "")).toBeNull();
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
