import type { ReactNode } from "react";
import { TriangleAlertIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";

/**
 * Why a Service's logs are off, with what to do about it. The logs still work some way (live output streams), so it
 * is the amber "needs attention" state. The action is an outline button or a link, never the surface's solid one.
 */
export function LogOffState({ title, children, action }: { title: string; children: ReactNode; action?: ReactNode }) {
  return <Alert role="status">
    <TriangleAlertIcon className="text-warning" />
    <AlertTitle>{title}</AlertTitle>
    <AlertDescription>{children}{action ? <> {action}</> : null}</AlertDescription>
  </Alert>;
}
