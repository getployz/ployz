import { createFileRoute } from "@tanstack/react-router";

// Pathless group for signed-out pages; LoginPanel sends sign-ins from here to /cloud.
export const Route = createFileRoute("/_public")({});
