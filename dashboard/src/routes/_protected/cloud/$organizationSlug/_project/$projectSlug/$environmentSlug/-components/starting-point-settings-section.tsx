import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import { useDeployStartingPoint } from "#/modules/deployments/deployment-commands";

/** A starting point runs nothing until it's deployed; from then on it's a running Branch. */
export function StartingPointSettingsSection(target: {
  organizationSlug: string; projectSlug: string; environmentSlug: string; environmentId: string;
}) {
  const deploy = useDeployStartingPoint(target);
  return (
    <section aria-labelledby="deployment-heading" className="flex flex-col gap-4">
      <h2 id="deployment-heading" className="text-lg font-semibold">Deployment</h2>
      <Field orientation="horizontal">
        <FieldContent>
          <FieldLabel>Not deployed</FieldLabel>
          <FieldDescription>Nothing runs here. Branches of it start from everything it describes.</FieldDescription>
          {deploy.isError && <FieldError>{deploy.error.message}</FieldError>}
        </FieldContent>
        <Button disabled={deploy.isPending} onClick={() => deploy.mutate()}>
          {deploy.isPending && <Spinner data-icon="inline-start" />}
          Deploy this environment
        </Button>
      </Field>
    </section>
  );
}
