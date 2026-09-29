import { PackageIcon, TerminalIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";

export function getServiceIcon(service: { source: { type: "empty" | "uploaded" | "git" | "image" } }) {
  switch (service.source.type) {
    case "empty":
    case "uploaded":
      return <TerminalIcon />;
    case "git":
      return <GitHubMarkIcon />;
    case "image":
      return <PackageIcon />;
  }
}

/** A card's status dot by its state. */
export function getServiceStatusClasses(state: "success" | "changed" | "warning" | "destructive" | undefined) {
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

  if (state === "warning") {
    return {
      dot: "bg-warning-soft",
      innerDot: "bg-warning",
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
