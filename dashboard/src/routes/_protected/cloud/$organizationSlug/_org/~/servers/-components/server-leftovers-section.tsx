import { useQuery } from "@tanstack/react-query";
import { CopyButton } from "#/components/copy-button";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { serverLeftovers } from "#/modules/volume-run/volume-run.leftovers.queries";

const WRITES = {
  true: "May hold writes no other server has",
  false: "Holds writes copied from another server",
  null: "Unknown whether it holds writes no other server has",
} as const;

/**
 * The copies deleted Volumes left on this Server that Ployz keeps, since only a person deletes them. Each row shows the
 * copy as the Server reports it now, and copies the command that deletes exactly that copy.
 */
export function ServerLeftoversSection({ organizationSlug, server }: { organizationSlug: string; server: { id: string; name: string } }) {
  const { data: leftovers = [] } = useQuery(serverLeftovers(organizationSlug, server.id));
  if (leftovers.length === 0) return null;
  return (
    <SettingsSection
      id="server-leftovers"
      title="Copies of deleted volumes"
      description="Kept when their volumes were deleted. Only you can delete them."
    >
      {leftovers.map((copy) => {
        const command = `ployz volume leftovers rm ${copy.docker_volume} --on ${server.name} --confirm ${copy.confirm}`;
        const taken = new Date(copy.newest.created_unix_seconds * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
        const size = copy.bound_bytes === null ? null : `up to ${(copy.bound_bytes / 1e9).toFixed(1)} GB`;
        return (
          <SettingsRow
            key={`${copy.docker_volume}-${copy.slot}`}
            label={<span className="font-mono">{copy.docker_volume}</span>}
            hint={[`Last snapshot ${taken}`, size, WRITES[`${copy.written_here}`]].filter((part) => part !== null).join(" · ")}
            commit="none"
            control={<CopyButton value={command} label="Copy delete command" showLabel variant="outline" size="sm" />}
          />
        );
      })}
    </SettingsSection>
  );
}
