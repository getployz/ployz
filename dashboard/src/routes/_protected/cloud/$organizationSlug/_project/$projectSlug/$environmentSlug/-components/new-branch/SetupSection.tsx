import { FieldDescription, FieldLegend, FieldSet } from "#/components/ui/field";
import type { SetupCommand } from "#/modules/project/tables";
import { listNames, type BranchPlan } from "#/modules/branches/branch-plan";
import { SetupCommandsField } from "./SetupCommandsField";

/** Setup Commands, only while the Branch gets New, empty data: they run once in its own services, to seed it. */
export function SetupSection({ plan, nameOf, setupCommands, onSetupCommands, onSetupBlur }: {
  plan: BranchPlan;
  nameOf: (lineage: string) => string;
  setupCommands: SetupCommand[];
  onSetupCommands: (next: SetupCommand[], typed?: boolean) => void;
  onSetupBlur?: () => void;
}) {
  const empty = plan.nodes.filter((node) => node.role === "own" && node.nodeType === "volume").map((node) => nameOf(node.lineageId));
  const ownServices = plan.nodes.filter((node) => node.role === "own" && node.nodeType === "service")
    .map((node) => ({ lineageId: node.lineageId, name: nameOf(node.lineageId) }));
  if (empty.length === 0 || ownServices.length === 0) return null;
  return (
    <FieldSet>
      <FieldLegend>Setup command</FieldLegend>
      <FieldDescription>Runs once, before the services start. Use it to seed {listNames(empty)}.</FieldDescription>
      <SetupCommandsField id="branch-setup-command" commands={setupCommands} services={ownServices} onChange={onSetupCommands} onBlur={onSetupBlur} />
    </FieldSet>
  );
}
