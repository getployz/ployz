import { Trash2Icon } from "lucide-react";
import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Card } from "#/components/ui/card";
import { Field, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { formatCount, formatPercent, goalRate, isPathPattern } from "./analytics-model";
import { type AnalyticsGoal, type AnalyticsScope, useCreateGoal, useDeleteGoal } from "./analytics.queries";

/** A goal counts the visitors who reached a path; `*` matches any run of characters. */
type AnalyticsGoalsProps = { scope: AnalyticsScope; goals: ReadonlyArray<AnalyticsGoal>; visitors: number };

export function AnalyticsGoals({ scope, goals, visitors }: AnalyticsGoalsProps) {
  const deleteGoal = useDeleteGoal(scope);
  return (
    <Card size="sm" className="min-w-0 gap-2 px-3" data-analytics-table="Goals">
      <h3 className="text-sm font-medium">Goals</h3>
      {goals.length > 0 ? (
        <Table className="table-fixed">
          <TableHeader>
            <TableRow>
              <TableHead>Goal</TableHead>
              <TableHead className="w-18 text-right">Visitors</TableHead>
              <TableHead className="w-16 text-right">Views</TableHead>
              <TableHead className="w-14 text-right">Rate</TableHead>
              <TableHead className="w-9" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {goals.map((goal) => (
              <TableRow key={goal.id}>
                <TableCell className="truncate" title={`${goal.name} ${goal.pathPattern}`}>
                  {goal.name} <span className="font-mono text-xs text-muted-foreground">{goal.pathPattern}</span>
                </TableCell>
                <TableCell className="text-right font-mono tabular-nums">{formatCount(goal.visitors)}</TableCell>
                <TableCell className="text-right font-mono tabular-nums">{formatCount(goal.conversions)}</TableCell>
                <TableCell className="text-right font-mono tabular-nums">{formatRate(goalRate(goal.visitors, visitors))}</TableCell>
                <TableCell>
                  <Button variant="ghost" size="icon-xs" aria-label={`Delete ${goal.name}`} disabled={goal.id.startsWith("pending:")}
                    onClick={() => deleteGoal.mutate(goal.id)}><Trash2Icon /></Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      ) : <p className="text-xs text-muted-foreground">Count visitors who reach a page, like /signup/done.</p>}
      <GoalForm scope={scope} />
    </Card>
  );
}

const formatRate = (rate: number | null) => rate === null ? "–" : formatPercent(rate);

function GoalForm({ scope }: { scope: AnalyticsScope }) {
  const createGoal = useCreateGoal(scope);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const add = () => {
    if (name.trim() === "") return setError("Name the goal");
    if (!isPathPattern(path)) return setError("A path starts with / and has no spaces");
    createGoal.mutate({ name: name.trim(), pathPattern: path });
    setName("");
    setPath("");
  };
  return (
    <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); add(); }}>
      <Field className="w-auto min-w-32 flex-1" data-invalid={error !== null || undefined}>
        <FieldLabel htmlFor="goal-name">Name</FieldLabel>
        <Input id="goal-name" value={name} placeholder="Signed up" autoComplete="off"
          onChange={(event) => { setName(event.target.value); setError(null); }} />
      </Field>
      <Field className="w-auto min-w-32 flex-1" data-invalid={error !== null || undefined}>
        <FieldLabel htmlFor="goal-path">Path</FieldLabel>
        <Input id="goal-path" value={path} placeholder="/signup/done" autoComplete="off" className="font-mono"
          onChange={(event) => { setPath(event.target.value); setError(null); }} />
      </Field>
      <Button type="submit" size="sm">Add goal</Button>
      {error ? <FieldError className="basis-full">{error}</FieldError> : null}
    </form>
  );
}
