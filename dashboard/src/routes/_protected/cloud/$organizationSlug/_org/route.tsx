import { createFileRoute } from "@tanstack/react-router";

// Pathless group for organization-wide pages; the organization layout renders the shell.
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org")({});
