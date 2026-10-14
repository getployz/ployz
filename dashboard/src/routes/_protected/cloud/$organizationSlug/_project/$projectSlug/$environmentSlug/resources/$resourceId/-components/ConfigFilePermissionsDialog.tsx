import { Schema } from "effect";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { FieldGroup } from "#/components/ui/field";
import { appFormOptions, showErrorsAfterBlurOrSubmit, useAppForm, validateOnChangeOrBlur } from "#/form";
import { strictParseOptions } from "#/lib/schema";
import { fileAccessDraftSchema, type FileAccess } from "#/modules/config-store/store-configs";

const permissionsFormOptions = appFormOptions.strictSchema({
  defaultValues: { mode: "", uid: "", gid: "" },
  errorVisibility: showErrorsAfterBlurOrSubmit,
  validators: [validateOnChangeOrBlur(Schema.toStandardSchemaV1(fileAccessDraftSchema, { parseOptions: strictParseOptions }))],
});

export function ConfigFilePermissionsDialog({ file, access, onClose, onSubmit }: {
  file: string; access: FileAccess; onClose: () => void; onSubmit: (access: FileAccess) => void;
}) {
  const form = useAppForm({
    ...permissionsFormOptions,
    defaultValues: Schema.encodeSync(fileAccessDraftSchema)(access),
    onSubmit: ({ schemaOutputs }) => {
      onSubmit(schemaOutputs[0]);
      onClose();
    },
  });

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form.AppForm>
          <form.Form className="flex flex-col gap-4">
            <DialogHeader>
              <DialogTitle>Permissions for {file}</DialogTitle>
              <DialogDescription>The mount stays read-only.</DialogDescription>
            </DialogHeader>
            <FieldGroup className="grid grid-cols-3 items-start">
              <form.Field name="mode">
                {(field) => <field.Text id="config-file-mode" label="Mode" className="font-mono" autoComplete="off" autoFocus />}
              </form.Field>
              <form.Field name="uid">
                {(field) => <field.Text id="config-file-uid" label="User ID" className="font-mono" inputMode="numeric" autoComplete="off" />}
              </form.Field>
              <form.Field name="gid">
                {(field) => <field.Text id="config-file-gid" label="Group ID" className="font-mono" inputMode="numeric" autoComplete="off" />}
              </form.Field>
            </FieldGroup>
            <DialogFooter>
              <DialogClose render={<Button type="button" variant="outline"
                // Keep focus on the input so Cancel doesn't trigger blur validation.
                onMouseDown={(event) => event.preventDefault()} />}>
                Cancel
              </DialogClose>
              <form.SubmitButton>Save</form.SubmitButton>
            </DialogFooter>
          </form.Form>
        </form.AppForm>
      </DialogContent>
    </Dialog>
  );
}
