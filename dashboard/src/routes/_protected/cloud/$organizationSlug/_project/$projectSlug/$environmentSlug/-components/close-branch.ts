import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { useStillHere } from "#/hooks/use-still-here";
import { toErrorMessage } from "#/lib/error-message";
import { closeBranchServerFn } from "#/modules/branches/branch-functions";

const joined = (names: readonly string[]) =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

/** What closing a Branch that isn't kept deletes: its Own Copies, which started empty. */
export const closeDeletes = (own: readonly string[]) =>
  own.length === 0 ? "Nothing runs here yet." : `Deletes its own ${own.length === 1 ? "copy" : "copies"} of ${joined(own)}.`;

/** How a Branch closes: at once while an open pull request would bring it back, after one plain confirm when nothing real goes, else typed. */
export function closeMode({ kept, hasBranches, isDefault, prOpen }: {
  kept: boolean; hasBranches: boolean; isDefault: boolean; prOpen: boolean;
}): "now" | "confirm" | "typed" {
  if (kept || hasBranches || isDefault) return "typed";
  return prOpen ? "now" : "confirm";
}

/** Closes a Branch that isn't kept, then goes to its Parent, unless the user moved on while it closed. */
export function useCloseBranch({ organizationSlug, projectSlug, environmentId, name, parentNamespace }: {
  organizationSlug: string;
  projectSlug: string;
  environmentId: string;
  name: string;
  parentNamespace: string;
}) {
  const closeBranch = useServerFn(closeBranchServerFn);
  const navigate = useNavigate();
  const markHere = useStillHere();
  const [closing, setClosing] = useState(false);

  async function close() {
    // Someone who moved on while it closed stays where they went.
    const stillHere = markHere();
    setClosing(true);
    try {
      await closeBranch({ data: { organizationSlug, environmentId } });
      toast(`Closing ${name}`);
      if (stillHere()) void navigate({
        to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
        params: { organizationSlug, projectSlug, environmentSlug: parentNamespace },
      });
    } catch (error) {
      toast.error(toErrorMessage(error, `Couldn't close ${name}.`));
      setClosing(false);
    }
  }

  return { close, closing };
}
