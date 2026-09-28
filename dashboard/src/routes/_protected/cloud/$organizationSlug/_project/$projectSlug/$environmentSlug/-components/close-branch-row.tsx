import { useState } from "react";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldTitle } from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { closeBranchServerFn } from "#/modules/branches/branch-functions";

const joined = (names: readonly string[]) =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

/**
 * Closes a Branch that isn't kept. A PR Environment with an open pull request closes at once, since the next push brings
 * it back; any other Branch asks once, naming its Own Copies, which started empty.
 */
export function CloseBranchRow({ organizationSlug, projectSlug, environmentId, name, parentNamespace, reopensWith, own }: {
  organizationSlug: string;
  projectSlug: string;
  environmentId: string;
  name: string;
  parentNamespace: string;
  /** The open pull request whose next push brings it back. */
  reopensWith: number | null;
  /** Its Own Copies' names. */
  own: readonly string[];
}) {
  const closeBranch = useServerFn(closeBranchServerFn);
  const navigate = useNavigate();
  const router = useRouter();
  const [confirming, setConfirming] = useState(false);
  const [closing, setClosing] = useState(false);
  const deletes = own.length === 0 ? "Nothing runs here yet." : `Deletes its own ${own.length === 1 ? "copy" : "copies"} of ${joined(own)}.`;

  async function close() {
    // Someone who moved on while it closed stays where they went.
    const from = router.state.location.state.key;
    setClosing(true);
    try {
      await closeBranch({ data: { organizationSlug, environmentId } });
      toast(`Closing ${name}`);
      if (router.state.location.state.key === from) void navigate({
        to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
        params: { organizationSlug, projectSlug, environmentSlug: parentNamespace },
      });
    } catch (error) {
      toast.error(toErrorMessage(error, `Couldn't close ${name}.`));
      setClosing(false);
    }
  }

  return (
    <>
      <Field orientation="horizontal">
        <FieldContent>
          <FieldTitle>Close {name}</FieldTitle>
          <FieldDescription>
            {reopensWith === null ? deletes : `It comes back with the next push to PR #${reopensWith}.`}
          </FieldDescription>
        </FieldContent>
        <Button variant="outline" disabled={closing} onClick={() => reopensWith === null ? setConfirming(true) : void close()}>
          {closing ? <Spinner data-icon="inline-start" /> : null}
          {reopensWith === null ? "Close branch" : "Close now"}
        </Button>
      </Field>
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={`Close ${name}?`}
        description={deletes}
        actionLabel="Close branch"
        variant="destructive"
        onConfirm={() => {
          setConfirming(false);
          return close();
        }}
      />
    </>
  );
}
