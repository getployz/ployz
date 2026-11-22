import { definePlugin } from "nitro";
import { exitOnBadConfig } from "./config.server";

// With a bad environment every route would 500 while /api/health answers ok, so web exits at boot instead.
export default definePlugin(exitOnBadConfig);
