import { describe, expect, it } from "vitest";
import {
  managedNetworkingFailureLabel,
  managedNetworkingProgressLabel,
} from "#/components/deployment-networking-presentation";

describe("managed networking deployment presentation", () => {
  it("names certificate and route-cutover progress", () => {
    expect(managedNetworkingProgressLabel("ensuring_certificates")).toBe(
      "Provisioning HTTPS certificates",
    );
    expect(managedNetworkingProgressLabel("route_cutover")).toBe(
      "Publishing routes to gateways",
    );
  });

  it("presents typed failures without arbitrary Core messages", () => {
    expect(
      managedNetworkingFailureLabel({
        kind: "automatic_hostname_collision",
        hostname: "api.example.test",
        routeBindingId: "binding-1",
      }),
    ).toBe("Automatic hostname api.example.test is already bound");
    expect(
      managedNetworkingFailureLabel({
        kind: "route_cutover_failed",
        hostname: "api.example.test",
        reason: { reason: "gateway_unavailable", machineId: "edge-1" },
      }),
    ).toBe("Route could not reach gateway edge-1");
  });

  it("ignores untyped failures", () => {
    expect(managedNetworkingFailureLabel({ kind: "planning_failed" })).toBeNull();
  });
});
