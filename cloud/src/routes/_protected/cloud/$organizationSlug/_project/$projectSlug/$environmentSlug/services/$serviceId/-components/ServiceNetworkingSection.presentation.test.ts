import { describe, expect, it } from "vitest";
import {
  canConfigureManagedHostname,
  customDomainCapabilityPresentation,
  customDomainDnsGuidance,
  managedHostnameRuntimePresentation,
} from "./ServiceNetworkingSection.presentation";

const publicUrl = {
  mode: "ployz" as const,
  domain: "brisk-river.up.ployz.app",
  leaseApex: "brisk-river.up.ployz.app",
  dnsTarget: {
    intent: "enabled" as const,
    allocation: "allocated" as const,
    publication: "applied" as const,
  },
};

const expectedHostname = "api.brisk-river.up.ployz.app";

describe("canConfigureManagedHostname", () => {
  it("allows configuration before runtime state is available", () => {
    expect(
      canConfigureManagedHostname({
        mode: "disabled",
        lensStatus: "unavailable",
      }),
    ).toBe(true);
  });

  it("respects a connected runtime that disables managed hostnames", () => {
    expect(
      canConfigureManagedHostname({
        mode: "disabled",
        lensStatus: "live_rows",
      }),
    ).toBe(false);
  });
});

describe("managedHostnameRuntimePresentation", () => {
  it("uses the exact automatic binding identity for TLS", () => {
    const presentation = present({
      bindings: [
        {
          id: "declared-binding",
          hostname: expectedHostname,
          origin: "declared",
          tls: { status: "available", certificateId: "wrong-certificate" },
        },
        {
          id: "managed-binding",
          hostname: expectedHostname,
          origin: "automatic",
          tls: { status: "unknown" },
        },
      ],
    });

    expect(presentation.route.observation).toEqual({
      status: "exact",
      binding: expect.objectContaining({ id: "managed-binding" }),
    });
    expect(presentation.route.label).toBe("Route current");
    expect(presentation.tls.label).toBe("TLS evidence unavailable");
  });

  it("keeps duplicate identity conflict distinct from an absent route", () => {
    const duplicate = {
      hostname: expectedHostname,
      origin: "automatic" as const,
      tls: { status: "unknown" as const },
    };
    const presentation = present({
      bindings: [
        { ...duplicate, id: "binding-a" },
        { ...duplicate, id: "binding-b" },
      ],
    });

    expect(presentation.route.observation.status).toBe("conflict");
    expect(presentation.route.label).toBe("Route identity conflict");
    expect(presentation.tls.label).toBe("TLS identity conflict");
  });

  it("presents the prior serving automatic binding and certificate after a failed replacement", () => {
    const presentation = present({
      bindings: [
        {
          id: "prior-binding",
          hostname: "api.old.up.ployz.app",
          origin: "automatic",
          tls: { status: "available", certificateId: "prior-certificate" },
        },
      ],
    });

    expect(presentation.route.observation).toEqual({
      status: "prior_serving",
      binding: expect.objectContaining({
        id: "prior-binding",
        hostname: "api.old.up.ployz.app",
        tls: { status: "available", certificateId: "prior-certificate" },
      }),
    });
    expect(presentation.route.label).toBe("Prior route preserved");
    expect(presentation.tls.label).toBe("Prior TLS preserved");
  });

  it("applies stale freshness to route, TLS, DNS, and every gateway state", () => {
    const presentation = present({
      runtimeStatus: "unavailable",
      bindings: [
        {
          id: "managed-binding",
          hostname: expectedHostname,
          origin: "automatic",
          tls: { status: "available", certificateId: "certificate-1" },
        },
      ],
      machines: [
        {
          id: "a",
          name: "edge-a",
          gateway: { status: "current", routeCount: 2 },
        },
        {
          id: "b",
          name: "edge-b",
          gateway: { status: "last_known_good", routeCount: 1 },
        },
        {
          id: "c",
          name: "edge-c",
          gateway: { status: "unavailable", routeCount: 0 },
        },
        {
          id: "d",
          name: "edge-d",
          gateway: { status: "silent", reason: "no_answer" },
        },
        { id: "e", name: "worker-e", gateway: { status: "not_installed" } },
      ],
    });

    expect(presentation.freshness).toEqual({ status: "stale" });
    expect(presentation.route.label).toBe("Route last observed");
    expect(presentation.tls.label).toBe("TLS availability last observed");
    expect(presentation.dns).toEqual({
      label: "DNS publication last observed applied",
      variant: "outline",
    });
    expect(presentation.gateways.map(({ status }) => status)).toEqual([
      "current",
      "last_known_good",
      "unavailable",
      "silent",
      "not_installed",
    ]);
    expect(
      presentation.gateways.every(
        ({ label }) =>
          label.includes("stale") || label.includes("last observed"),
      ),
    ).toBe(true);
    expect(presentation.gateways[0]?.label).not.toContain("current");
    expect(
      presentation.gateways.every(({ variant }) => variant === "outline"),
    ).toBe(true);
  });

  it("shows typed DNS target progress without inferring publication", () => {
    const presentation = present({
      publicUrl: {
        ...publicUrl,
        domain: null,
        dnsTarget: {
          intent: "enabled",
          allocation: "unacquired",
          publication: "unpublished",
        },
      },
    });

    expect(presentation.dns.label).toBe("DNS target pending");
  });
});

describe("customDomainCapabilityPresentation", () => {
  it("allows self-hosted custom-domain controls without billing language", () => {
    expect(
      customDomainCapabilityPresentation({
        status: "ready",
        billingMode: "self_hosted",
        currentPlan: null,
        hasActivePaidSubscription: false,
      }),
    ).toEqual({
      status: "allowed",
      canAddOrReplace: true,
      showUpgrade: false,
    });
  });

  it.each(["solo", "teams"] as const)(
    "allows an active hosted %s plan",
    (currentPlan) => {
      expect(
        customDomainCapabilityPresentation({
          status: "ready",
          billingMode: "hosted",
          currentPlan,
          hasActivePaidSubscription: true,
        }),
      ).toEqual({
        status: "allowed",
        canAddOrReplace: true,
        showUpgrade: false,
      });
    },
  );

  it("offers a Solo upgrade only for confirmed hosted Free", () => {
    expect(
      customDomainCapabilityPresentation({
        status: "ready",
        billingMode: "hosted",
        currentPlan: "free",
        hasActivePaidSubscription: false,
      }),
    ).toEqual({
      status: "blocked",
      canAddOrReplace: false,
      message: "Custom domains are available on Solo and Teams.",
      showUpgrade: true,
    });
  });

  it.each([
    [{ status: "loading" as const }, "Checking custom-domain access…"],
    [
      { status: "unavailable" as const },
      "We couldn’t check your plan. Try again.",
    ],
    [
      {
        status: "ready" as const,
        billingMode: "hosted" as const,
        currentPlan: null,
        hasActivePaidSubscription: false,
      },
      "We couldn’t check your plan. Try again.",
    ],
    [
      {
        status: "ready" as const,
        billingMode: "hosted" as const,
        currentPlan: "solo" as const,
        hasActivePaidSubscription: false,
      },
      "We couldn’t check your plan. Try again.",
    ],
  ])("fails closed without optimistic upgrade language", (input, message) => {
    expect(customDomainCapabilityPresentation(input)).toEqual({
      status: "blocked",
      canAddOrReplace: false,
      message,
      showUpgrade: false,
    });
  });
});

describe("customDomainDnsGuidance", () => {
  it("presents the exact allocated lease as manual CNAME/ALIAS guidance", () => {
    expect(
      customDomainDnsGuidance({
        mode: "ployz",
        leaseApex: "tenant.up.ployz.app",
        lensStatus: "live_rows",
      }),
    ).toEqual({
      kind: "target",
      target: "tenant.up.ployz.app",
      title: "Configure DNS manually",
      description:
        "Create a CNAME record to this target, or use your provider's ALIAS or ANAME record at the zone apex.",
    });
  });

  it.each([
    [
      {
        mode: "ployz" as const,
        leaseApex: null,
        lensStatus: "live_rows" as const,
      },
      "No Ployz DNS target is allocated for this runtime. Save the route now and configure DNS separately.",
    ],
    [
      {
        mode: "custom" as const,
        leaseApex: "ignored.up.ployz.app",
        lensStatus: "live_rows" as const,
      },
      "This runtime uses custom public networking. Save the route and manage DNS with your provider.",
    ],
    [
      {
        mode: "disabled" as const,
        leaseApex: null,
        lensStatus: "live_rows" as const,
      },
      "Public URL automation is disabled. Save the route and manage DNS and routing outside Ployz.",
    ],
  ])("does not invent a target for manual modes", (input, description) => {
    expect(customDomainDnsGuidance(input)).toEqual({
      kind: "manual",
      title: "DNS is managed separately",
      description,
    });
  });

  it.each(["no_connection", "connecting", "unavailable", "unreachable"] as const)(
    "does not interpret default public URL state while runtime is %s",
    (lensStatus) => {
      expect(
        customDomainDnsGuidance({
          mode: "disabled",
          leaseApex: null,
          lensStatus,
        }),
      ).toEqual({
        kind: "manual",
        title: "DNS target unavailable right now",
        description:
          "Runtime evidence is not current, so Ployz cannot show a DNS target. Save the route now and configure DNS separately.",
      });
    },
  );
});

function present(
  overrides: Partial<Parameters<typeof managedHostnameRuntimePresentation>[0]>,
) {
  return managedHostnameRuntimePresentation({
    expectedHostname,
    runtimeStatus: "live_rows",
    publicUrl,
    bindings: [],
    machines: [],
    ...overrides,
  });
}
