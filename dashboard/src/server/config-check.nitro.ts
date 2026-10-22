import { definePlugin } from "nitro";
import { startupConfigFailure } from "./config.server";

// With a bad environment every route would 500 while /api/health answers ok, so web exits at boot instead.
export default definePlugin(() => {
  const failure = startupConfigFailure();
  if (failure === undefined) return;
  console.error(failure);
  process.exit(1);
});
