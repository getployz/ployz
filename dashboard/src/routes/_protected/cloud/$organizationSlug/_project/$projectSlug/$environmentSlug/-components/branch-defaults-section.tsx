import { useState } from "react";
import { Badge } from "#/components/ui/badge";
import { Field, FieldDescription, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import { useBranchSetupDefaults } from "#/modules/branches/branch.collection";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import type { SetupCommand } from "#/modules/project/tables";
import { SetupCommandsField } from "./new-branch/SetupCommandsField";

/** What a new Branch of this Environment starts with. Saved at once, like Deployment Policy, never staged. */
export function BranchDefaultsSection({ organizationSlug, environmentId }: { organizationSlug: string; environmentId: string }) {
  const document = useEnvironmentDocument(organizationSlug, environmentId);
  const lineageName = useLineageNames(organizationSlug);
  const save = useBranchSetupDefaults(organizationSlug);
  const saved = document?.branchSetupCommands ?? [];
  const [draft, setDraft] = useState<SetupCommand[] | null>(null);
  if (!document) return null;
  const commands = draft ?? saved;
  // The Working State's services; a removed service keeps its row until deployed away.
  const own = document.intent.services.map((node) => ({ lineageId: node.lineageId, name: lineageName(node.lineageId, environmentId) }));

  // Blank commands stay in the draft until typed; only whole ones save, and only when they changed. A draft with nothing
  // left unsaved goes, so the saved (optimistic) rows show again and a failed save's rollback shows too.
  function commit(next: SetupCommand[]) {
    const whole = next.filter((setup) => setup.command.trim()).map((setup) => ({ lineageId: setup.lineageId, command: setup.command.trim() }));
    if (JSON.stringify(whole) !== JSON.stringify(saved)) save(environmentId, whole);
    if (whole.length === next.length) setDraft(null);
  }

  return (
    <section aria-labelledby="branch-defaults-heading" className="flex flex-col gap-4">
      <div>
        <h2 id="branch-defaults-heading" className="text-lg font-semibold">Branches of {document.name}</h2>
        <p className="text-sm text-muted-foreground">What a new branch of {document.name} starts with.</p>
      </div>
      {document.intent.volumes.length > 0 && (
        <FieldSet>
          <FieldLegend variant="label">Data</FieldLegend>
          <RadioGroup value="empty">
            <FieldLabel htmlFor="branch-defaults-empty">
              <Field orientation="horizontal"><RadioGroupItem value="empty" id="branch-defaults-empty" />Start empty</Field>
            </FieldLabel>
            <FieldLabel htmlFor="branch-defaults-copy">
              <Field orientation="horizontal" data-disabled="true">
                <RadioGroupItem value="copy" id="branch-defaults-copy" disabled />
                Copy {document.name}'s data<Badge variant="secondary">Soon</Badge>
              </Field>
            </FieldLabel>
          </RadioGroup>
        </FieldSet>
      )}
      <Field>
        <FieldLabel htmlFor="branch-defaults-setup">Then run</FieldLabel>
        {own.length === 0 ? <FieldDescription>Add a service to run commands in.</FieldDescription> : <>
          <SetupCommandsField id="branch-defaults-setup" commands={commands} services={own}
            onChange={(next, typed) => { setDraft(next); if (!typed) commit(next); }}
            onBlur={() => commit(commands)} />
          <FieldDescription>Prefills every new branch. Each runs in the service's new image before it first starts, until it deploys once.</FieldDescription>
        </>}
      </Field>
    </section>
  );
}
