import { useRouter } from "@tanstack/react-router";
import { CircleAlertIcon } from "lucide-react";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import { Button } from "#/components/ui/button";

export function RouteErrorAlert({
  title,
  description,
}: {
  title: string;
  description: string;
}) {
  const router = useRouter();

  return (
    <Alert variant="destructive">
      <CircleAlertIcon />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{description}</AlertDescription>
      <AlertAction>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => {
            void router.invalidate();
          }}
        >
          Retry
        </Button>
      </AlertAction>
    </Alert>
  );
}
