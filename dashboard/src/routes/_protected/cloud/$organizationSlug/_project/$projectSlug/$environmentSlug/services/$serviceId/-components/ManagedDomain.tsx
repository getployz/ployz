import { Effect, Schema, SchemaGetter, SchemaIssue } from "effect";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { FieldGroup } from "#/components/ui/field";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateOnChangeOrBlur,
} from "#/form";
import { parseServiceSetting, type ServiceManagedHostname } from "@ployz/sdk/config";
import { strictParseOptions } from "#/lib/schema";
import { domainPortSchema } from "./domain-port";

/** A field core admits: Effect owns the form's envelope, Rust the rule. */
function coreSetting<T>(parse: <Input>(value: Input) => T) {
  const decoded = Schema.declare<T>((value): value is T => {
    try { parse(value); return true; } catch { return false; }
  });
  return decoded.pipe(Schema.decodeTo(decoded, {
    decode: SchemaGetter.transformOrFail((value, options) => Effect.try({
      try: () => parse(value),
      catch: (cause) => new SchemaIssue.InvalidValue({ message: cause instanceof Error ? cause.message : "Invalid domain" }, undefined, options),
    })),
    encode: SchemaGetter.transform((value) => value),
  }));
}
const serviceManagedHostnamePrefixSchema = coreSetting((value) => parseServiceSetting("managedHostnamePrefix", value));
const serviceManagedHostnameSchema = coreSetting((value) => parseServiceSetting("managedHostnameValue", value));

export function ManagedDomainDialog({
  mode = "edit",
  managed,
  clusterDomain,
  takenPrefixes,
  defaultTargetPort,
  onClose,
  onSubmit,
}: {
  /** `port` edits only the port: a Store generated domain keeps its prefix. */
  mode?: "edit" | "generate" | "port";
  managed: ServiceManagedHostname;
  clusterDomain: string | null;
  takenPrefixes: string[];
  defaultTargetPort: number | null;
  onClose: () => void;
  onSubmit: (next: ServiceManagedHostname) => void;
}) {
  const taken = new Set(takenPrefixes);
  const schema = Schema.toStandardSchemaV1(
    Schema.Struct({
      prefix: serviceManagedHostnamePrefixSchema.check(
        Schema.makeFilter<string>((value) =>
          taken.has(value) ? "This subdomain is already in use." : undefined
        )
      ),
      port: domainPortSchema,
    }).pipe(
      Schema.decodeTo(serviceManagedHostnameSchema, {
        decode: SchemaGetter.transform(({ prefix, port }) => ({
          prefix,
          targetPort: port,
        })),
        encode: SchemaGetter.transform(({ prefix, targetPort }) => ({
          prefix,
          port: targetPort,
        })),
      })
    ),
    { parseOptions: strictParseOptions }
  );
  const form = useAppForm({
    ...appFormOptions.strictSchema({
      defaultValues: {
        prefix: managed.prefix,
        port: managed.targetPort === null ? "" : String(managed.targetPort),
      },
      errorVisibility: showErrorsAfterBlurOrSubmit,
      validators: [validateOnChangeOrBlur(schema)],
    }),
    // Optimistic: saving rolls back and toasts on failure.
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
              <DialogTitle>
                {mode === "generate"
                  ? "Generate Service Domain"
                  : mode === "port" ? "Edit generated domain" : "Edit managed domain"}
              </DialogTitle>
              <DialogDescription>
                {mode === "edit"
                  ? "Update your domain or target port."
                  : "Enter the port your app is listening on."}
              </DialogDescription>
            </DialogHeader>
            <FieldGroup>
              {mode === "edit" ? (
                <form.Field name="prefix">
                  {(field) => (
                    <field.Text
                      label="Subdomain"
                      className="font-mono"
                      description={
                        clusterDomain
                          ? `.${clusterDomain}`
                          : "Your domain is assigned on your first deploy."
                      }
                    />
                  )}
                </form.Field>
              ) : null}
              <form.Field name="port">
                {(field) => (
                  <field.Text
                    label={mode === "generate" ? "Port" : "Target port"}
                    type="number"
                    inputMode="numeric"
                    min={1}
                    max={65535}
                    step={1}
                    placeholder={
                      defaultTargetPort === null
                        ? "Uses PORT"
                        : String(defaultTargetPort)
                    }
                    description="Leave blank to use PORT."
                  />
                )}
              </form.Field>
            </FieldGroup>
            <DialogFooter>
              <DialogClose
                render={
                  <Button
                    type="button"
                    variant="outline"
                    onMouseDown={(event) => event.preventDefault()}
                  />
                }
              >
                Cancel
              </DialogClose>
              <form.SubmitButton>
                {mode === "generate" ? "Generate Domain" : "Save domain"}
              </form.SubmitButton>
            </DialogFooter>
          </form.Form>
        </form.AppForm>
      </DialogContent>
    </Dialog>
  );
}
