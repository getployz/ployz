import type { ServiceListing } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { storeServiceStatus } from "./StoreServiceNode";

const service = { source: "image", change: null, template: null, id: "web", name: "web", private_dns: "web" } as unknown as ServiceListing;

describe("storeServiceStatus", () => {
  it("says Not running once the Servers' evidence has none of it, never Deployed", () => {
    expect(storeServiceStatus(service, 0, null, false, true).text).toBe("Not running");
    expect(storeServiceStatus(service, 0, null, false, false).text).toBe("Deployed");
  });
});
