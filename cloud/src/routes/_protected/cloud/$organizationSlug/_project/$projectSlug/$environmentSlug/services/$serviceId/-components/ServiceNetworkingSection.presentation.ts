import type {
  RuntimeGatewayTestimony,
  RuntimeLensStatus,
  RuntimePublicUrl,
  RuntimeRouteBinding,
} from "#/modules/runtime/runtime.collection";
import type { BillingPlan } from "#/modules/billing/tables";

export type CustomDomainDnsGuidance = ReturnType<
  typeof customDomainDnsGuidance
>;

export function canConfigureManagedHostname(input: {
  mode: RuntimePublicUrl["mode"];
  lensStatus: RuntimeLensStatus;
}) {
  const runtimeIsCurrent =
    input.lensStatus === "live_empty" || input.lensStatus === "live_rows";
  return !runtimeIsCurrent || input.mode !== "disabled";
}

export function customDomainDnsGuidance(
  publicUrl: Pick<RuntimePublicUrl, "mode" | "leaseApex"> & {
    lensStatus: RuntimeLensStatus;
  },
) {
  if (
    publicUrl.lensStatus !== "live_empty" &&
    publicUrl.lensStatus !== "live_rows"
  ) {
    return {
      kind: "manual" as const,
      title: "DNS target unavailable right now",
      description:
        "Runtime evidence is not current, so Ployz cannot show a DNS target. Save the route now and configure DNS separately.",
    };
  }
  if (publicUrl.mode === "ployz" && publicUrl.leaseApex !== null) {
    return {
      kind: "target" as const,
      target: publicUrl.leaseApex,
      title: "Configure DNS manually",
      description:
        "Create a CNAME record to this target, or use your provider's ALIAS or ANAME record at the zone apex.",
    };
  }

  const description =
    publicUrl.mode === "ployz"
      ? "No Ployz DNS target is allocated for this runtime. Save the route now and configure DNS separately."
      : publicUrl.mode === "custom"
        ? "This runtime uses custom public networking. Save the route and manage DNS with your provider."
        : "Public URL automation is disabled. Save the route and manage DNS and routing outside Ployz.";
  return {
    kind: "manual" as const,
    title: "DNS is managed separately",
    description,
  };
}

type CustomDomainBillingPresentationInput =
  | { status: "loading" }
  | { status: "unavailable" }
  | {
      status: "ready";
      billingMode: "hosted" | "self_hosted";
      currentPlan: BillingPlan | null;
      hasActivePaidSubscription: boolean;
    };

export type CustomDomainCapabilityPresentation =
  | {
      status: "allowed";
      canAddOrReplace: true;
      showUpgrade: false;
    }
  | {
      status: "blocked";
      canAddOrReplace: false;
      message: string;
      showUpgrade: boolean;
    };

export function customDomainCapabilityPresentation(
  input: CustomDomainBillingPresentationInput,
): CustomDomainCapabilityPresentation {
  if (input.status === "loading") {
    return {
      status: "blocked",
      canAddOrReplace: false,
      message: "Checking custom-domain access…",
      showUpgrade: false,
    };
  }
  if (input.status === "unavailable") {
    return {
      status: "blocked",
      canAddOrReplace: false,
      message: "We couldn’t check your plan. Try again.",
      showUpgrade: false,
    };
  }
  if (input.billingMode === "self_hosted") {
    return {
      status: "allowed",
      canAddOrReplace: true,
      showUpgrade: false,
    };
  }
  if (
    input.hasActivePaidSubscription &&
    (input.currentPlan === "solo" || input.currentPlan === "teams")
  ) {
    return {
      status: "allowed",
      canAddOrReplace: true,
      showUpgrade: false,
    };
  }
  if (input.currentPlan === "free") {
    return {
      status: "blocked",
      canAddOrReplace: false,
      message: "Custom domains are available on Solo and Teams.",
      showUpgrade: true,
    };
  }
  return {
    status: "blocked",
    canAddOrReplace: false,
    message: "We couldn’t check your plan. Try again.",
    showUpgrade: false,
  };
}

export type NetworkingBadgeVariant =
  "secondary" | "info" | "success" | "warning" | "destructive" | "outline";

type GatewayInput = {
  id: string;
  name: string;
  gateway: RuntimeGatewayTestimony;
};

export type RuntimeEvidenceFreshness =
  { status: "current" } | { status: "stale" };

export type ManagedRouteObservation =
  | { status: "absent" }
  | { status: "exact"; binding: RuntimeRouteBinding }
  | { status: "prior_serving"; binding: RuntimeRouteBinding }
  | { status: "conflict"; bindings: RuntimeRouteBinding[] };

export function managedHostnameRuntimePresentation(input: {
  expectedHostname: string | null;
  runtimeStatus: RuntimeLensStatus;
  publicUrl: RuntimePublicUrl;
  bindings: readonly RuntimeRouteBinding[];
  machines: readonly GatewayInput[];
}) {
  const freshness = runtimeEvidenceFreshness(input.runtimeStatus);
  const observation = managedRouteObservation(
    input.expectedHostname,
    input.bindings,
  );

  return {
    freshness,
    route: routePresentation(observation, freshness),
    tls: tlsPresentation(observation, freshness),
    dns: dnsPresentation(input.publicUrl, freshness),
    gateways: input.machines.map((machine) => ({
      id: machine.id,
      name: machine.name,
      status: machine.gateway.status,
      ...gatewayPresentation(machine.gateway, freshness),
    })),
  };
}

function runtimeEvidenceFreshness(
  status: RuntimeLensStatus,
): RuntimeEvidenceFreshness {
  return status === "live_rows" || status === "live_empty"
    ? { status: "current" }
    : { status: "stale" };
}

function managedRouteObservation(
  expectedHostname: string | null,
  bindings: readonly RuntimeRouteBinding[],
): ManagedRouteObservation {
  const automatic = bindings.filter(
    (binding) => binding.origin === "automatic",
  );
  const exact = expectedHostname
    ? automatic.filter((binding) => binding.hostname === expectedHostname)
    : [];

  if (exact.length > 1) {
    return { status: "conflict", bindings: exact };
  }
  const [exactBinding] = exact;
  if (exactBinding) {
    return { status: "exact", binding: exactBinding };
  }
  const [priorBinding] = automatic;
  if (automatic.length === 1 && priorBinding) {
    return { status: "prior_serving", binding: priorBinding };
  }
  if (automatic.length > 1) {
    return { status: "conflict", bindings: automatic };
  }
  return { status: "absent" };
}

function routePresentation(
  observation: ManagedRouteObservation,
  freshness: RuntimeEvidenceFreshness,
) {
  if (freshness.status === "stale") {
    switch (observation.status) {
      case "absent":
        return {
          observation,
          label: "Route evidence unavailable",
          variant: "outline" as const,
        };
      case "exact":
        return {
          observation,
          label: "Route last observed",
          variant: "outline" as const,
        };
      case "prior_serving":
        return {
          observation,
          label: "Prior route last observed",
          variant: "outline" as const,
        };
      case "conflict":
        return {
          observation,
          label: "Route conflict last observed",
          variant: "outline" as const,
        };
    }
  }

  switch (observation.status) {
    case "absent":
      return {
        observation,
        label: "Route pending",
        variant: "secondary" as const,
      };
    case "exact":
      return {
        observation,
        label: "Route current",
        variant: "success" as const,
      };
    case "prior_serving":
      return {
        observation,
        label: "Prior route preserved",
        variant: "warning" as const,
      };
    case "conflict":
      return {
        observation,
        label: "Route identity conflict",
        variant: "destructive" as const,
      };
  }
}

function tlsPresentation(
  observation: ManagedRouteObservation,
  freshness: RuntimeEvidenceFreshness,
) {
  if (observation.status === "absent") {
    return freshness.status === "current"
      ? { label: "TLS awaiting route", variant: "secondary" as const }
      : { label: "TLS evidence unavailable", variant: "outline" as const };
  }
  if (observation.status === "conflict") {
    return freshness.status === "current"
      ? { label: "TLS identity conflict", variant: "destructive" as const }
      : { label: "TLS conflict last observed", variant: "outline" as const };
  }

  const { binding } = observation;
  const prior = observation.status === "prior_serving";
  if (freshness.status === "stale") {
    switch (binding.tls.status) {
      case "available":
        return {
          label: prior
            ? "Prior TLS availability last observed"
            : "TLS availability last observed",
          variant: "outline" as const,
        };
      case "unavailable":
        return {
          label: "TLS unavailability last observed",
          variant: "outline" as const,
        };
      case "unknown":
        return {
          label: "TLS evidence unavailable when last observed",
          variant: "outline" as const,
        };
    }
  }
  switch (binding.tls.status) {
    case "available":
      return prior
        ? { label: "Prior TLS preserved", variant: "warning" as const }
        : { label: "TLS available", variant: "success" as const };
    case "unavailable":
      return { label: "TLS unavailable", variant: "warning" as const };
    case "unknown":
      return { label: "TLS evidence unavailable", variant: "outline" as const };
  }
}

function dnsPresentation(
  publicUrl: RuntimePublicUrl,
  freshness: RuntimeEvidenceFreshness,
) {
  if (freshness.status === "current") {
    return currentDnsPresentation(publicUrl);
  }
  return staleDnsPresentation(publicUrl);
}

function staleDnsPresentation(publicUrl: RuntimePublicUrl) {
  const { dnsTarget } = publicUrl;
  if (dnsTarget.intent === "disabled") {
    return {
      label: "DNS intent last observed disabled",
      variant: "outline" as const,
    };
  }
  if (dnsTarget.allocation === "unacquired") {
    return {
      label: "DNS allocation last observed pending",
      variant: "outline" as const,
    };
  }
  switch (dnsTarget.publication) {
    case "unpublished":
      return {
        label: "DNS publication last observed pending",
        variant: "outline" as const,
      };
    case "applied":
      return {
        label: "DNS publication last observed applied",
        variant: "outline" as const,
      };
    case "withdrawn":
      return {
        label: "DNS publication last observed withdrawn",
        variant: "outline" as const,
      };
  }
}

function currentDnsPresentation(publicUrl: RuntimePublicUrl) {
  const { dnsTarget } = publicUrl;
  if (dnsTarget.intent === "disabled") {
    return { label: "Managed DNS disabled", variant: "outline" as const };
  }
  if (dnsTarget.allocation === "unacquired") {
    return { label: "DNS target pending", variant: "secondary" as const };
  }
  switch (dnsTarget.publication) {
    case "unpublished":
      return {
        label: "DNS publication pending",
        variant: "secondary" as const,
      };
    case "applied":
      return { label: "DNS published", variant: "success" as const };
    case "withdrawn":
      return { label: "DNS withdrawn", variant: "warning" as const };
  }
}

function gatewayPresentation(
  gateway: RuntimeGatewayTestimony,
  freshness: RuntimeEvidenceFreshness,
): {
  label: string;
  detail: string;
  variant: NetworkingBadgeVariant;
} {
  const current = currentGatewayPresentation(gateway);
  if (freshness.status === "current") {
    return current;
  }
  return staleGatewayPresentation(gateway);
}

function staleGatewayPresentation(gateway: RuntimeGatewayTestimony) {
  switch (gateway.status) {
    case "current":
      return {
        label: "Gateway testimony stale",
        detail: `${routeCount(gateway.routeCount)} when last observed serving`,
        variant: "outline" as const,
      };
    case "last_known_good":
      return {
        label: "Gateway LKG testimony stale",
        detail: routeCount(gateway.routeCount),
        variant: "outline" as const,
      };
    case "unavailable":
      return {
        label: "Gateway unavailable when last observed",
        detail: routeCount(gateway.routeCount),
        variant: "outline" as const,
      };
    case "silent":
      return {
        label: "Gateway silence last observed",
        detail:
          gateway.reason === "no_answer"
            ? "Server did not answer"
            : "Server answered without gateway testimony",
        variant: "outline" as const,
      };
    case "not_installed":
      return {
        label: "Gateway role absence last observed",
        detail: "This server had no gateway role",
        variant: "outline" as const,
      };
  }
}

function currentGatewayPresentation(gateway: RuntimeGatewayTestimony) {
  switch (gateway.status) {
    case "current":
      return {
        label: "Gateway current",
        detail: routeCount(gateway.routeCount),
        variant: "success" as const,
      };
    case "last_known_good":
      return {
        label: "Gateway last known good",
        detail: routeCount(gateway.routeCount),
        variant: "warning" as const,
      };
    case "unavailable":
      return {
        label: "Gateway unavailable",
        detail: routeCount(gateway.routeCount),
        variant: "destructive" as const,
      };
    case "silent":
      return {
        label: "Gateway silent",
        detail:
          gateway.reason === "no_answer"
            ? "Server did not answer"
            : "Server answered without gateway testimony",
        variant: "outline" as const,
      };
    case "not_installed":
      return {
        label: "Gateway not installed",
        detail: "This server has no gateway role",
        variant: "outline" as const,
      };
  }
}

function routeCount(count: number) {
  return `${count} ${count === 1 ? "route" : "routes"}`;
}
