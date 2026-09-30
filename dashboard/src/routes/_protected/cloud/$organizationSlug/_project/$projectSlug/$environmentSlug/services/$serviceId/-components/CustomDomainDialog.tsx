import { Schema, SchemaGetter } from "effect";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { useEmbeddedCheckout } from "#/modules/billing/use-embedded-checkout";
import { ArrowDownIcon, CheckIcon, LockIcon, ZapIcon } from "lucide-react";
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
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "#/components/ui/sheet";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateOnChangeOrBlur,
} from "#/form";
import { strictParseOptions } from "#/lib/schema";
import { domainPortSchema } from "./domain-port";

/** A public DNS name with a dot: labels of letters, digits and inner hyphens. The Store checks the rest. */
const HOSTNAME = /^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?$/u;

/** A custom domain as the dialog edits it: a blank port follows the container's PORT. */
export type CustomDomain = { hostname: string; targetPort: number | null };

const customDomainFormSchema = Schema.toStandardSchemaV1(
  Schema.Struct({
    hostname: Schema.Trim.check(
      Schema.isNonEmpty({ message: "Enter a hostname." }),
      Schema.makeFilter<string>((hostname) => HOSTNAME.test(hostname.toLowerCase())
        ? undefined : "Enter a domain you own, like api.example.com."),
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

export function CustomDomainDialog({
  route,
  initial,
  hostnameFixed = false,
  defaultTargetPort,
  onClose,
  onSubmit,
}: {
  route?: CustomDomain;
  /** A new domain's typed values, as before the Store refused them. */
  initial?: CustomDomain;
  /** Editing changes only the port: a Store domain is addressed by its hostname. */
  hostnameFixed?: boolean;
  defaultTargetPort: number | null;
  onClose: () => void;
  onSubmit: (next: CustomDomain) => void;
}) {
  const form = useAppForm({
    ...customDomainFormOptions,
    defaultValues: {
      hostname: (route ?? initial)?.hostname ?? "",
      port: (route ?? initial)?.targetPort == null ? "" : String((route ?? initial)?.targetPort),
    },
    // Saving happens in the background and toasts on failure.
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


const PRO_PERKS = [
  { title: "HTTPS handled", detail: "Certificates issued and renewed for you." },
  { title: "As many as you need", detail: "Apex, subdomains, every service, every environment." },
  { title: "Point one DNS record", detail: "We show you exactly what to add." },
];

/** What "Custom Domain" opens when the Organization has no Pro: the pitch and Polar's checkout, not a form it would refuse. */
export function CustomDomainUpsellSheet({
  organizationSlug,
  service,
  environment,
  paid,
  onPaid,
  onClose,
}: {
  organizationSlug: string;
  service: string;
  environment: string | null;
  /** Paid, and waiting for Pro to reach Cloud. */
  paid: boolean;
  onPaid: () => void;
  onClose: () => void;
}) {
  const checkout = useEmbeddedCheckout(organizationSlug, () => {
    toast.success("You're on Pro. Thanks for supporting Ployz!");
    onPaid();
  });
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="gap-0 data-[side=right]:sm:max-w-md">
        <SheetHeader className="p-6 pb-2">
          <SheetTitle className="text-lg">Use your own domain</SheetTitle>
          <SheetDescription>
            Serve <span className="font-mono">{service}</span> from a domain you own.
          </SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-6 px-6 py-4">
          <div className="flex flex-col items-center gap-2 rounded-xl border bg-muted/40 p-4">
            <div className="flex w-full items-center gap-2 rounded-lg border bg-background px-3 py-2">
              <LockIcon className="size-4 text-success" />
              <span className="font-mono text-sm">api.yourcompany.com</span>
            </div>
            <ArrowDownIcon className="size-4 text-muted-foreground" />
            <div className="flex w-full items-center gap-2 rounded-lg border bg-background px-3 py-2">
              <ZapIcon className="size-4" />
              <span className="font-mono text-sm">{service}</span>
              {environment ? <span className="ml-auto text-muted-foreground text-sm">{environment}</span> : null}
            </div>
          </div>
          <ul className="flex flex-col gap-3">
            {PRO_PERKS.map((perk) => (
              <li key={perk.title} className="flex gap-3">
                <CheckIcon className="mt-0.5 size-4 shrink-0" />
                <div>
                  <div className="font-medium">{perk.title}</div>
                  <div className="text-muted-foreground text-sm">{perk.detail}</div>
                </div>
              </li>
            ))}
          </ul>
        </div>
        <SheetFooter className="border-t p-6">
          <div className="flex items-baseline justify-between">
            <span className="text-muted-foreground">Ployz Pro</span>
            <span><span className="font-semibold text-xl">$9</span><span className="text-muted-foreground"> / month</span></span>
          </div>
          <Button type="button" size="lg" disabled={checkout.pending || paid} onClick={() => void checkout.openCheckout()}>
            {checkout.pending || paid ? <Spinner data-icon="inline-start" /> : null}
            {paid ? "Finishing your upgrade…" : "Upgrade to Pro"}
          </Button>
          <p className="text-center text-muted-foreground text-xs">Everything else stays free on your servers.</p>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
