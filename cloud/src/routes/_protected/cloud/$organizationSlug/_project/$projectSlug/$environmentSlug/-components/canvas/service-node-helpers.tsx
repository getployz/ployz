import { PackageIcon, TerminalIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import type { ServiceDeploymentSurfaceState } from "#/modules/services/service-deployment-semantics";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";

export function getServiceIcon(service: EnvironmentServiceViewRecord["service"]) {
  switch (service.source.type) {
    case "empty":
      return <TerminalIcon />;
    case "git":
      return <GitHubMarkIcon />;
    case "image":
      return <PackageIcon />;
  }
}

export function getServiceSubtitle(service: EnvironmentServiceViewRecord["service"]) {
  switch (service.source.type) {
    case "empty":
      return null;
    case "git":
      return service.source.repository;
    case "image":
      return service.source.image;
  }
}

export function getServiceStatusClasses(state: ServiceDeploymentSurfaceState) {
  if (state === "success") {
    return {
      dot: "bg-success-soft",
      innerDot: "bg-success",
    };
  }

  if (state === "changed") {
    return {
      dot: "bg-changed-soft",
      innerDot: "bg-changed",
    };
  }

  if (state === "destructive") {
    return {
      dot: "bg-destructive-soft",
      innerDot: "bg-destructive",
    };
  }

  return {
    dot: "bg-muted",
    innerDot: "bg-muted-foreground",
  };
}
