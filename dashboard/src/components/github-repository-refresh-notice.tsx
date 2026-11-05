import { Alert, AlertDescription } from "#/components/ui/alert";
import { GITHUB_REPOS_POLL_MS } from "#/modules/github/github.collection";

/** The open picker's poll reads again, so the notice says when rather than offering a Retry. */
const AGAIN = `Trying again every ${GITHUB_REPOS_POLL_MS / 1000} seconds.`;

export function GithubRepositoryRefreshNotice({ initial = false }: { initial?: boolean }) {
  return (
    <Alert>
      <AlertDescription>
        {initial
          ? `Could not load repositories. ${AGAIN}`
          : `Could not refresh repositories. Shown results may be out of date. ${AGAIN}`}
      </AlertDescription>
    </Alert>
  );
}
