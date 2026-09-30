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
