import { createContext } from "react";

/** Where the open agent hosts the page's bottom bar, above its composer. Null while the agent is closed. */
export const AgentDock = createContext<HTMLElement | null>(null);
