import type { EnvironmentListing } from "@ployz/sdk";

/**
 * The Environments a preview may copy, each with why it never may: production and Protected Environments hold the
 * secrets pull request code nobody reviewed must never run with. Branches, previews and PR Environments among them,
 * aren't listed: a preview copies an Environment that is no Branch.
 */
export function startFromChoices(environments: readonly EnvironmentListing[]) {
  return environments
    .filter((environment) => environment.parent === null && environment.removal === null)
    .map((environment) => ({
      name: environment.name,
      never: environment.protected ? "Protected" : environment.name === "production" ? "not allowed" : null,
    }));
}
