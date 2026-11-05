import { type AnyRouter, createMemoryHistory } from "@tanstack/react-router";

/**
 * Pages whose URL is itself a secret, an invite, a claim link or a device code, mark their route `staticData.secret`.
 * A path counts as secret exactly when the router would render one of those routes for it, encodings and all.
 */
export const secretPagesOf = (router: AnyRouter) => (path: string) => {
  const { pathname } = router.parseLocation(createMemoryHistory({ initialEntries: [path] }).location);
  return router.getMatchedRoutes(pathname)[0].some((route) => route.options.staticData?.secret === true);
};
