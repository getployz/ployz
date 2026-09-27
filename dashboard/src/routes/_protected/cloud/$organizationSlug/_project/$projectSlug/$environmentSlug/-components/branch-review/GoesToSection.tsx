import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import { CheckIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { FieldDescription, FieldError } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { ItemGroup } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { presentRow } from "#/modules/branches/branch-review";
import type { BranchReviewView, PullRequest } from "#/modules/branches/use-branch-review";
import { useConditionalSave } from "#/modules/pr-environments/conditional-save-commands";
import { approverName } from "#/modules/pr-environments/pr-check";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";
import { RowPicks, useRowPicks } from "./RowPicks";

type Landing = BranchReviewView["goesTo"][number];

/**
 * What a PR Environment's pull request would move into one Destination when it merges, change by change, each with a tick
 * and each variable a value choice. Nothing moves until someone approves; once approved, the rows are held as they were,
 * and a row still missing its new value can be given one.
 */
export function GoesToSection({ review, pr, landing, name, environmentId }: {
  review: BranchReviewView;
  pr: PullRequest;
  landing: Landing;
  /** The PR Environment's name and id. */
  name: string;
  environmentId: string;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const destination = landing.destination.name;
  const save = useConditionalSave({ organizationSlug: params.organizationSlug, prEnvironmentId: environmentId, destinationEnvironmentId: landing.destination.id });
  const rows = useRowPicks(landing.rows, review.nameOf);
  const { approval } = landing;
  const failed = save.approve.error ?? save.withdraw.error;
  const count = approval ? approval.rows.length : landing.rows.length;
  return (
    <ReviewSection title={`Goes to ${destination} when #${pr.number} merges`} count={count}>
      {approval ? <HeldRows approval={approval} review={review} save={save} destination={destination} /> : landing.rows.length ? (
        <RowPicks picks={rows} names={{ from: name, parent: review.parent.name, destination }} verb={`Send to ${destination}`} />
      ) : <p className="text-sm text-muted-foreground">Nothing here that {destination} doesn't have.</p>}
      {!(approval || landing.rows.length) ? null
        : pr.closed ? <FieldDescription>#{pr.number} is closed, so nothing here goes to {destination}.</FieldDescription>
        : (
        <div className="flex flex-col gap-2">
          {approval ? (
            <Button className="self-start" variant="outline" disabled={save.withdraw.isPending} onClick={() => save.withdraw.mutate()}>
              {save.withdraw.isPending ? <Spinner data-icon="inline-start" /> : <CheckIcon data-icon="inline-start" />}
              Approved by {approverName(approval.approvedBy)} · Undo
            </Button>
          ) : (
            <Button className="self-start" disabled={rows.ticked.length === 0 || save.approve.isPending}
              onClick={() => save.approve.mutate({ review: landing.review, picks: rows.sent })}>
              {save.approve.isPending ? <Spinner data-icon="inline-start" /> : <CheckIcon data-icon="inline-start" />}Approve for {destination}
            </Button>
          )}
          <FieldDescription>
            {approval ? `When #${pr.number} merges, ${destination} deploys the code and these together.`
              : rows.ticked.length === 0 ? "Tick a change to approve."
              : `Nothing moves until someone approves. Unticked changes stay in ${name}.`}
          </FieldDescription>
          {failed ? <FieldError>{failed.message}</FieldError> : null}
        </div>
      )}
    </ReviewSection>
  );
}

/** The approved rows as they were approved; a row still missing its new value takes one here. */
function HeldRows({ approval, review, save, destination }: {
  approval: ConditionalSaveRow;
  destination: string;
  review: BranchReviewView;
  save: ReturnType<typeof useConditionalSave>;
}) {
  return (
    <ItemGroup className="gap-1">
      {approval.rows.map(({ row, missing }) => {
        const presented = presentRow(row, review.nameOf);
        return (
          <div key={row.key} className="flex flex-col gap-1">
            <ChangeRowItem row={presented} />
            {missing ? <GiveValue destination={destination} label={presented.label} secret={row.role === "move" && row.choice?.secret === true} save={save} rowKey={row.key} /> : null}
          </div>
        );
      })}
    </ItemGroup>
  );
}

function GiveValue({ destination, label, secret, save, rowKey }: { destination: string; label: string; secret: boolean; save: ReturnType<typeof useConditionalSave>; rowKey: string }) {
  const [value, setValue] = useState("");
  return (
    <form className="flex flex-wrap gap-1" onSubmit={(event) => {
      event.preventDefault();
      save.give.mutate({ key: rowKey, value });
    }}>
      <Input className="min-w-48 flex-1" type={secret ? "password" : "text"} autoComplete="off" value={value}
        aria-label={`New value of ${label}`} placeholder={`${destination}'s value`} onChange={(event) => setValue(event.target.value)} />
      <Button type="submit" variant="outline" disabled={!value || save.give.isPending}>
        {save.give.isPending ? <Spinner data-icon="inline-start" /> : null}Save value
      </Button>
      {save.give.error ? <FieldError>{save.give.error.message}</FieldError> : null}
    </form>
  );
}
