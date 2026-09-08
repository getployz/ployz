import {
  decodeStrict,
  strictParseOptions,
} from "#/modules/environment-design/schema";
import { useState, type ReactNode } from "react";
import { CircleAlertIcon } from "lucide-react";
import { Schema, SchemaGetter } from "effect";
import type { ServiceRoute } from "#/modules/environment-design/tables";
import { asRecord } from "#/lib/json";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
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
  validateAfterBlurThenWhileInvalid,
} from "#/form";
import { serviceRouteSchema } from "#/modules/environment-design/services";

const portStringSchema = Schema.String.check(
  Schema.makeFilter((value) => {
    const port = Number(value.trim());
    return Number.isInteger(port) && port >= 1 && port <= 65_535;
  }, { message: "Enter a port between 1 and 65535." }),
);

const customDomainFormSchema = Schema.toStandardSchemaV1(
  Schema.Struct({
    hostname: Schema.Trim.check(
      Schema.isNonEmpty({ message: "Enter a hostname." }),
    ),
    port: portStringSchema,
  }).pipe(
    Schema.decodeTo(
      Schema.Struct({
        hostname: Schema.String,
        targetPort: Schema.Int.check(
          Schema.isBetween({ minimum: 1, maximum: 65_535 }),
        ),
      }),
      {
        decode: SchemaGetter.transform(({ hostname, port }) => ({
          hostname,
          targetPort: Number(port),
        })),
        encode: SchemaGetter.transform(({ hostname, targetPort }) => ({
          hostname,
          port: String(targetPort),
        })),
      },
    ),
  ),
  { parseOptions: strictParseOptions },
);

const customDomainFormOptions = appFormOptions.strictSchema({
  defaultValues: { hostname: "", port: "" },
  errorVisibility: showErrorsAfterBlurOrSubmit,
  validators: [validateAfterBlurThenWhileInvalid(customDomainFormSchema)],
});

type SaveFailure = {
  kind: "capability" | "persistence";
  message: string;
};

function isCustomDomainCapabilityError<T>(
  error: T,
): error is T & { _tag: "CustomDomainCapabilityError"; message?: string } {
  return asRecord(error)?.["_tag"] === "CustomDomainCapabilityError";
}

function errorMessage<T>(error: T) {
  return error instanceof Error
    ? error.message
    : "The custom domain could not be saved.";
}

export function CustomDomainDialog({
  route,
  defaultTargetPort,
  capabilityAction,
  onCapabilityRejected,
  onClose,
  onSubmit,
}: {
  route?: ServiceRoute;
  defaultTargetPort: number;
  capabilityAction: ReactNode;
  onCapabilityRejected: () => Promise<void>;
  onClose: () => void;
  onSubmit: (next: ServiceRoute) => Promise<void>;
}) {
  const [saveFailure, setSaveFailure] = useState<SaveFailure | null>(null);

  const form = useAppForm({
    ...customDomainFormOptions,
    defaultValues: {
      hostname: route?.hostname ?? "",
      port: String(route?.targetPort ?? defaultTargetPort),
    },
    onSubmit: async ({ schemaOutputs }) => {
      setSaveFailure(null);
      try {
        await onSubmit(
          decodeStrict(serviceRouteSchema, {
            id: route?.id ?? crypto.randomUUID(),
            ...schemaOutputs[0],
          }),
        );
        onClose();
      } catch (error) {
        if (isCustomDomainCapabilityError(error)) {
          setSaveFailure({
            kind: "capability",
            message:
              error.message ??
              "Your custom-domain access changed. Review billing before saving.",
          });
          try {
            await onCapabilityRejected();
          } catch {
            // The authoritative save failure remains visible if billing refresh fails.
          }
          return;
        }
        setSaveFailure({ kind: "persistence", message: errorMessage(error) });
      }
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
                Saving stages this route. Apply changes separately from the
                canvas.
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
                    inputMode="numeric"
                    placeholder="8080"
                  />
                )}
              </form.Field>
            </FieldGroup>
            {saveFailure ? (
              <Alert variant="destructive">
                <CircleAlertIcon />
                <AlertTitle>
                  {saveFailure.kind === "capability"
                    ? "Custom domain access changed"
                    : "Custom domain not saved"}
                </AlertTitle>
                <AlertDescription>{saveFailure.message}</AlertDescription>
                {saveFailure.kind === "capability" ? (
                  <AlertAction>{capabilityAction}</AlertAction>
                ) : null}
              </Alert>
            ) : null}
            <DialogFooter>
              <DialogClose render={<Button type="button" variant="outline" />}>
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
