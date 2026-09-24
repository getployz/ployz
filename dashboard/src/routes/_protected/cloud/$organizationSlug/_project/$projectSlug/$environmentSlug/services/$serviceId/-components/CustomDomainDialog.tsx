import {
  decodeStrict,
  strictParseOptions,
} from "#/modules/environment-design/schema";
import { useState } from "react";
import { CircleAlertIcon } from "lucide-react";
import { Schema, SchemaGetter } from "effect";
import type { ServiceRoute } from "#/modules/environment-design/tables";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
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
import { serviceRouteSchema } from "#/modules/environment-design/services";
import { domainPortSchema } from "./domain-port";

const customDomainFormSchema = Schema.toStandardSchemaV1(
  Schema.Struct({
    hostname: Schema.Trim.check(
      Schema.isNonEmpty({ message: "Enter a hostname." })
    ),
    port: domainPortSchema,
  }).pipe(
    Schema.decodeTo(
      Schema.Struct({
        hostname: Schema.String,
        targetPort: Schema.NullOr(
          Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 65_535 }))
        ),
      }),
      {
        decode: SchemaGetter.transform(({ hostname, port }) => ({
          hostname,
          targetPort: port,
        })),
        encode: SchemaGetter.transform(({ hostname, targetPort }) => ({
          hostname,
          port: targetPort,
        })),
      }
    )
  ),
  { parseOptions: strictParseOptions }
);

const customDomainFormOptions = appFormOptions.strictSchema({
  defaultValues: { hostname: "", port: "" },
  errorVisibility: showErrorsAfterBlurOrSubmit,
  validators: [validateOnChangeOrBlur(customDomainFormSchema)],
});

function errorMessage<T>(error: T) {
  return error instanceof Error
    ? error.message
    : "The custom domain could not be saved.";
}

export function CustomDomainDialog({
  route,
  defaultTargetPort,
  onClose,
  onSubmit,
}: {
  route?: ServiceRoute;
  defaultTargetPort: number | null;
  onClose: () => void;
  onSubmit: (next: ServiceRoute) => void;
}) {
  const [saveFailure, setSaveFailure] = useState<string | null>(null);

  const form = useAppForm({
    ...customDomainFormOptions,
    defaultValues: {
      hostname: route?.hostname ?? "",
      port: route?.targetPort == null ? "" : String(route.targetPort),
    },
    onSubmit: ({ schemaOutputs }) => {
      setSaveFailure(null);
      let next: ServiceRoute;
      try {
        next = decodeStrict(serviceRouteSchema, { id: route?.id ?? crypto.randomUUID(), ...schemaOutputs[0] });
      } catch (error) {
        setSaveFailure(errorMessage(error));
        return;
      }
      // Optimistic: saving rolls back and toasts on failure.
      onSubmit(next);
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
                {route ? "Edit custom domain" : "Add custom domain"}
              </DialogTitle>
              <DialogDescription>
                Point a domain you own at this service.
              </DialogDescription>
            </DialogHeader>
            <FieldGroup>
              <form.Field name="hostname">
                {(field) => (
                  <field.Text
                    id="custom-domain-hostname"
                    label="Domain"
                    className="font-mono"
                    placeholder="api.example.com"
                  />
                )}
              </form.Field>
              <form.Field name="port">
                {(field) => (
                  <field.Text
                    id="custom-domain-target-port"
                    label="Target port"
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
            {saveFailure ? (
              <Alert variant="destructive">
                <CircleAlertIcon />
                <AlertTitle>Custom domain not saved</AlertTitle>
                <AlertDescription>{saveFailure}</AlertDescription>
              </Alert>
            ) : null}
            <DialogFooter>
              <DialogClose
                render={
                  <Button
                    type="button"
                    variant="outline"
                    // Keep focus on the input so Cancel doesn't trigger blur validation.
                    onMouseDown={(event) => event.preventDefault()}
                  />
                }
              >
                Cancel
              </DialogClose>
              <form.SubmitButton>Save route</form.SubmitButton>
            </DialogFooter>
          </form.Form>
        </form.AppForm>
      </DialogContent>
    </Dialog>
  );
}
