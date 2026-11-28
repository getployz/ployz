import type { EnvironmentListing } from "@ployz/sdk";

/**
 * The Environments a preview may copy, each with why it never may: production and Protected Environments hold the
 * secrets pull request code nobody reviewed must never run with. Previews themselves aren't listed.
 */
export function startFromChoices(environments: readonly EnvironmentListing[], previews: ReadonlySet<string>) {
  return environments
    .filter((environment) => !previews.has(environment.name) && environment.removal === null)
    .map((environment) => ({
      name: environment.name,
      never: environment.protected ? "Protected" : environment.name === "production" ? "not allowed" : null,
    }));
}
