import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { readStore } from "./config-store.server";

/** The Namespaces the Organization's Environments own, as the Store names them. A failed read fails: nothing reads as unowned. */
export const ownedNamespaces = (organizationId: string) =>
  readStore(organizationId, { query: "namespaces" }).pipe(Effect.map(({ namespaces }) => namespaces.map(({ namespace }) => namespace)));
