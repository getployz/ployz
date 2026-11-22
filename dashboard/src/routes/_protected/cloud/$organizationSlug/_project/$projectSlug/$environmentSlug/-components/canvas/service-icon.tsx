import { PackageIcon, TerminalIcon } from "lucide-react";
import type { ServiceListing } from "@ployz/sdk";
import { GitHubMarkIcon } from "#/components/icons/github-mark";

/** The glyph for where a Service's image comes from. */
export function serviceIcon(source: ServiceListing["source"]) {
  switch (source) {
    case "empty":
    case "uploaded":
      return <TerminalIcon />;
    case "git":
      return <GitHubMarkIcon />;
    case "image":
      return <PackageIcon />;
  }
}
