import { Badge } from "#/components/ui/badge";
import { Field, FieldDescription, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import { useBranchSetupDefaults } from "#/modules/branches/branch.collection";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { SetupCommandsField, useSavedSetupCommands } from "./new-branch/SetupCommandsField";

/** What a new Branch of this Environment starts with. Saved at once, like Deployment Policy, never staged. */
export function BranchDefaultsSection({ organizationSlug, environmentId }: { organizationSlug: string; environmentId: string }) {
  const document = useEnvironmentDocument(organizationSlug, environmentId);
  const lineageName = useLineageNames(organizationSlug);
  const save = useBranchSetupDefaults(organizationSlug);
  const setup = useSavedSetupCommands(document?.branchSetupCommands ?? [], (whole) => save(environmentId, whole));
  if (!document) return null;
  // The Working State's services; a removed service keeps its row until deployed away.
  const own = document.intent.services.map((node) => ({ lineageId: node.lineageId, name: lineageName(node.lineageId, environmentId) }));

  return (
    <section aria-labelledby="branch-defaults-heading" className="flex flex-col gap-4">
      <h2 id="branch-defaults-heading" className="text-lg font-semibold">Branches of {document.name}</h2>
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
          <SetupCommandsField id="branch-defaults-setup" commands={setup.commands} services={own}
            onChange={setup.onChange} onBlur={setup.onBlur} />
          <FieldDescription>Runs once, before the service first starts.</FieldDescription>
        </>}
      </Field>
    </section>
  );
}
