import { plural } from "#/lib/plural";
import type { Claim } from "#/modules/identity/claim.queries";

export function ClaimSummary({ claim }: { claim: Extract<Claim, { state: "pending" }> }) {
  return (
    <p className="text-sm text-muted-foreground">
      An agent made it. {plural(claim.servers, "Server")} and {plural(claim.projects, "Project")}, gone in {plural(claim.hoursLeft, "hour")} unless someone keeps it.
    </p>
  );
}
