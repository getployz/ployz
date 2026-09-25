import {
  GlobeIcon,
  PencilIcon,
  Trash2Icon,
} from "lucide-react";
import { Schema, SchemaGetter } from "effect";
import { Link } from "@tanstack/react-router";
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
import { FieldDescription, FieldGroup } from "#/components/ui/field";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateOnChangeOrBlur,
} from "#/form";
import type { ServiceManagedHostname } from "#/modules/environment-design/tables";
import { managedHostname } from "#/modules/environment-design/managed-service-exports";
import {
  serviceManagedHostnamePrefixSchema,
  serviceManagedHostnameSchema,
} from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";
import {
  CertificateEvidence,
  DomainRowShell,
  type DomainCertificateEvidence,
  DomainTitle,
} from "./domain-row";
import { domainPortSchema } from "./domain-port";

export function ManagedDomainRow({
  organizationSlug,
  managed,
  clusterDomain,
  certificateEvidence,
  defaultTargetPort,
  changed,
  onEdit,
  onDelete,
}: {
  organizationSlug: string;
  managed: ServiceManagedHostname;
  clusterDomain: string | null;
  certificateEvidence: DomainCertificateEvidence;
  defaultTargetPort: number | null;
  changed: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const hostname = clusterDomain ? managedHostname(managed.prefix, clusterDomain) : null;
  const port = managed.targetPort ?? defaultTargetPort;
  return (
    <div className="flex flex-col gap-1">
      <DomainRowShell
        changed={changed}
        icon={<GlobeIcon />}
        actions={
          <>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Edit managed domain"
              onClick={onEdit}
            >
              <PencilIcon />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Remove managed domain"
              onClick={onDelete}
            >
              <Trash2Icon />
            </Button>
          </>
        }
      >
        {hostname ? (
          <DomainTitle hostname={hostname} copyLabel="Copy domain" />
        ) : (
          <div className="truncate font-mono text-sm">
            {managed.prefix}
            <span className="text-muted-foreground">.pending</span>
          </div>
        )}
        <div className="text-muted-foreground text-sm">
          → {port === null ? "Uses PORT" : `Port ${port}`}
        </div>
      </DomainRowShell>
      {hostname ? null : (
        <FieldDescription>
          The Cluster Domain is pending.{" "}
          <Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }}>
            Open Server Settings
          </Link>
        </FieldDescription>
      )}
      <CertificateEvidence evidence={certificateEvidence} />
    </div>
  );
}

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
                  : "Edit managed domain"}
              </DialogTitle>
              <DialogDescription>
                {mode === "generate"
                  ? "Enter the port your app is listening on."
                  : "Update your domain or target port."}
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
                          : "The Cluster Domain is pending."
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
