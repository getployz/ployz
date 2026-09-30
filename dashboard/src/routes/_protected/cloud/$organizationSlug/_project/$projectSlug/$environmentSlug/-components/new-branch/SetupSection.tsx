import { FieldDescription, FieldLegend, FieldSet } from "#/components/ui/field";
import type { BranchPlanView } from "@ployz/sdk";
import type { SetupCommand } from "#/modules/config-store/branch-picks";
import { listNames } from "#/lib/plural";
import { SetupCommandsField } from "./SetupCommandsField";

/** Setup Commands, only while the Branch gets New, empty data: they run once in its own services, to seed it. */
export function SetupSection({ plan, nameOf, setupCommands, onSetupCommands, onSetupBlur }: {
  plan: BranchPlanView;
  nameOf: (lineage: string) => string;
  setupCommands: SetupCommand[];
  onSetupCommands: (next: SetupCommand[], typed?: boolean) => void;
  onSetupBlur?: () => void;
}) {
  const empty = plan.nodes.filter((node) => node.role === "own" && node.kind === "volume").map((node) => nameOf(node.name));
  const ownServices = plan.nodes.filter((node) => node.role === "own" && node.kind === "service")
    .map((node) => ({ lineageId: node.name, name: nameOf(node.name) }));
  if (empty.length === 0 || ownServices.length === 0) return null;
  return (
    <FieldSet>
      <FieldLegend>Setup command</FieldLegend>
      <FieldDescription>Runs once, before the services start. Use it to seed {listNames(empty)}.</FieldDescription>
      <SetupCommandsField id="branch-setup-command" commands={setupCommands} services={ownServices} onChange={onSetupCommands} onBlur={onSetupBlur} />
    </FieldSet>
  );
}
