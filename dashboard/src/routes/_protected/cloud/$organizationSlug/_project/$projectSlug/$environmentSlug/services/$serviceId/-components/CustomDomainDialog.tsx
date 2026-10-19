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
import { strictParseOptions } from "#/lib/schema";
import { NOT_A_HOSTNAME, storedHostname } from "#/modules/config-store/store-hostname";
import { domainPortSchema } from "./domain-port";

/** A public DNS name with a dot: labels of letters, digits and inner hyphens. The Store checks the rest. */
const HOSTNAME = /^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?$/u;

const isAscii = (hostname: string) => /^\p{ASCII}*$/u.test(hostname);

const isAsciiHostname = (hostname: string) => isAscii(hostname) && HOSTNAME.test(hostname.toLowerCase());

const isUnicodeNameForTheStore = (hostname: string) => !isAscii(hostname) && hostname.includes(".");

const isHostname = (hostname: string) =>
  !NOT_A_HOSTNAME.test(hostname) && (isAsciiHostname(hostname) || isUnicodeNameForTheStore(hostname));

/** A custom domain as the dialog edits it: a blank port follows the container's PORT. */
export type CustomDomain = { hostname: string; targetPort: number | null };

/** ASCII lowercase; the Store turns the rest to punycode itself. */
const submittedHostname = (hostname: string) => isAscii(hostname) ? hostname.toLowerCase() : hostname;

const customDomainFormSchema = (taken: ReadonlySet<string>) => Schema.toStandardSchemaV1(
  Schema.Struct({
    hostname: Schema.Trim.check(
      Schema.isNonEmpty({ message: "Enter a hostname." }),
      Schema.makeFilter<string>((hostname) => isHostname(hostname)
        ? undefined : "Enter a domain you own, like api.example.com."),
      Schema.makeFilter<string>((hostname) => taken.has(storedHostname(hostname))
        ? "Already on this service." : undefined),
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
          hostname: submittedHostname(hostname),
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

export function CustomDomainDialog({
  route,
  initial,
  refusal,
  hostnameFixed = false,
  takenHostnames = [],
  defaultTargetPort,
  onClose,
  onSubmit,
}: {
  route?: CustomDomain;
  /** A new domain's typed values, as before the Store refused them. */
  initial?: CustomDomain;
  /** Why the Store refused `initial`, shown until the hostname changes. */
  refusal?: string;
  /** Editing changes only the port: a Store domain is addressed by its hostname. */
  hostnameFixed?: boolean;
  /** The service's other custom domains, listed or still being added: adding one again would change nothing. */
  takenHostnames?: readonly string[];
  defaultTargetPort: number | null;
  onClose: () => void;
  onSubmit: (next: CustomDomain) => void;
}) {
  const form = useAppForm({
    ...appFormOptions.strictSchema({
      defaultValues: {
        hostname: (route ?? initial)?.hostname ?? "",
        port: (route ?? initial)?.targetPort == null ? "" : String((route ?? initial)?.targetPort),
      },
      errorVisibility: showErrorsAfterBlurOrSubmit,
      validators: [validateOnChangeOrBlur(customDomainFormSchema(new Set(takenHostnames.map(storedHostname))))],
    }),
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
                    disabled={hostnameFixed && route !== undefined}
                    placeholder="api.example.com"
                    error={field.value === initial?.hostname ? refusal : undefined}
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
                    description={
                      defaultTargetPort === null
                        ? "Leave blank to use PORT, 8080 unless set."
                        : "Leave blank to use PORT."
                    }
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
