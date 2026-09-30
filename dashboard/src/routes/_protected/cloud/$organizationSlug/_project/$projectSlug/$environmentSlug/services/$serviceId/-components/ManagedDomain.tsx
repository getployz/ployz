import { Schema, SchemaGetter } from "effect";
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

/** Why core refuses `value`, in its words, or undefined when it admits it. */
function coreRefusal(parse: (value: unknown) => unknown, value: unknown) {
  try { parse(value); return undefined; } catch (cause) { return cause instanceof Error ? cause.message : "Invalid domain"; }
}

/** A field core admits: Effect owns the form's envelope, Rust the rule and its message. */
function coreSetting<T>(parse: <Input>(value: Input) => T) {
  const admitted = (value: unknown): value is T => coreRefusal(parse, value) === undefined;
  // The input side lets anything through to the check, so a refusal shows core's reason, not the declaration's
  // generic "Expected <Declaration>".
  const input = Schema.declare<T>((_value): _value is T => true).check(Schema.makeFilter((value) => coreRefusal(parse, value)));
  return input.pipe(Schema.decodeTo(Schema.declare<T>(admitted), {
    decode: SchemaGetter.transform((value) => parse(value)),
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
  mode?: "edit" | "generate";
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
                  : "Edit generated domain"}
              </DialogTitle>
              <DialogDescription>
                {mode === "edit"
                  ? "Change its subdomain or the port it reaches."
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
