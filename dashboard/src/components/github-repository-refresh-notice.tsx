import { Alert, AlertDescription } from "#/components/ui/alert";

export function GithubRepositoryRefreshNotice({ initial = false }: { initial?: boolean }) {
  return (
    <Alert>
      <AlertDescription>
        {initial
          ? "Could not load repositories. Retrying automatically."
          : "Could not refresh repositories. Shown results may be out of date. Retrying automatically."}
      </AlertDescription>
    </Alert>
  );
}
