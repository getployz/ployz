---
date: 2026-06-10
topic: route-protection
---

# Route Protection Requirements

## Summary

Ployz should let a route be public or protected at the gateway without making the protected service itself Cloud-aware. The v1 shape is explicit route protection on route bindings, with generic Access Provider protection for browser-based private routes and Cloud-owned presets resolved before requests reach core.

---

## Problem Frame

Dashboard-embedded tools and private previews need safe external URLs without forcing each upstream service to implement authentication. The core must support that without becoming Ployz Cloud, BetterAuth, or an org-membership system. The gateway should admit requests only when the route's current protection can be enforced, and product surfaces should hide the access machinery behind simple choices such as public or Cloud-private.

---

## Key Decisions

- **Protection belongs to route bindings.** A service, namespace, or serving target is not protected by itself; only ingress through a specific route binding is protected.
- **Core stores normalized protection.** Product-facing presets such as "protect with Cloud" are resolved by Cloud or another caller before core stores route protection.
- **Access Providers stay generic.** Core knows a cluster-scoped Access Provider and an opaque Access Requirement, not Cloud orgs, SSO groups, projects, or BetterAuth sessions.
- **Deploy route specs are authoritative.** If a deploy spec omits a route, the route is detached; if it includes a route but omits protection, that route is normalized to public.
- **Password is modeled but deferred.** Password protection remains a route protection concept, but implementation waits for a real secret-material story.

---

## Actors

- **Operator:** Uses CLI, SDK, or Cloud product surfaces to deploy services and manage route exposure.
- **Cloud or product surface:** Translates ergonomic presets into normalized core route protection.
- **Access Provider:** Authenticates a requester and decides whether that requester satisfies an Access Requirement.
- **Gateway:** Enforces route protection from current or last-known-good route/provider/session state.
- **Requester:** A browser user visiting a protected route directly or through an embedded dashboard iframe.
- **Upstream service:** Receives proxied traffic only after gateway ingress protection passes.

---

## Requirements

**Route Protection Model**

- R1. Every route binding must have explicit route protection in core state.
- R2. Core route protection must include `Public` and `AccessProvider` in v1.
- R3. Password protection may exist in the conceptual model, but it must not be enabled until password material can be stored and delivered without turning route state into a secret sink.
- R4. Route protection must affect only gateway ingress through the route binding.
- R5. Internal service reachability, namespace membership, and serving target eligibility must not be inferred from route protection.

**Access Provider Model**

- R6. Access Provider records must be cluster-scoped gateway infrastructure.
- R7. Access Provider records must carry enough non-secret display and trust metadata for gateways to start browser access flows and verify Access Grants.
- R8. Access Provider records must not store Cloud session secrets, BetterAuth secrets, password material, or other provider-owned secret credentials in ordinary route state.
- R9. Route protection may reference an Access Provider only after that provider exists and is usable.
- R10. Normal Access Provider removal must be rejected while route protections still reference that provider.
- R11. Access Provider updates may occur while referenced only when dependent route protections remain enforceable.

**Deploy And Route Operations**

- R12. Deploy inputs that include routes must be authoritative for the route set they submit.
- R13. A route omitted from the next deploy spec must be detached by that deploy operation.
- R14. A route included in a deploy spec with omitted protection must be normalized to `Public`.
- R15. A route included in a deploy spec with protection must replace the current protection exactly.
- R16. A standalone route-protection operation must require an explicit target protection value.
- R17. Route detach and public route protection must remain distinct outcomes.
- R18. Mutating provider, route, and protection changes must be explicit operations with operation evidence.

**Gateway Enforcement**

- R19. Provider-protected routes must fail closed when the referenced provider is missing, disabled, unusable, or no longer matches the route's current protection.
- R20. Access Grants must be short-lived, single-use evidence for one route binding and the current route protection/provider state.
- R21. One Access Grant must create or refresh at most one Route Access Session.
- R22. Route Access Sessions must be route-binding scoped and accepted across gateway replicas.
- R23. Route Access Sessions must stop applying when route protection, Access Provider state, or route binding identity changes.
- R24. Serving target changes and upstream deploys must not invalidate Route Access Sessions unless route protection or route binding identity changes.
- R25. Route host changes must require fresh Route Access Sessions.
- R26. Route Access Sessions may be bearer cookies in v1, using host-only browser cookies and no device binding.
- R27. Gateway Session Keys must be local secret authority for route session admission, not ordinary route state or operation history.

**Browser Experience**

- R28. Direct browser navigation to a protected Access Provider route must show a gateway-owned protected-route page before provider sign-in.
- R29. The gateway page must use minimal Access Provider display metadata such as provider name, not provider-supplied arbitrary HTML.
- R30. The Access Provider must own sign-in, account selection, SSO, and rich authorization explanations.
- R31. Dashboard iframe access must support Embed Access Renewal so an embedded route can obtain fresh access evidence without a manual sign-in click when the embedding product can provide it.
- R32. Stale grants and sessions must fail closed, but dashboard/embed presentation may retry once against current route protection.
- R33. API, fetch, and other non-navigation requests must receive status responses rather than login pages or provider redirects.
- R34. Gateway presentation must use request context conservatively, returning page/redirect flows for document navigation and status responses otherwise.
- R35. Gateway-owned paths under `/.ployz/*` must be reserved and never proxied to upstream services.
- R36. No user-visible route logout is required in v1.

**Evidence And Privacy**

- R37. Operation evidence for route protection changes must be explicit enough to show whether a route became public, provider-protected, or rejected.
- R38. Operation evidence must not dump full opaque Access Requirement values when a summarized or fingerprinted representation is enough for inspection.
- R39. Provider-scoped requester identity may exist inside Route Access Sessions for audit and per-user behavior, but it must remain opaque to core.
- R40. Requester identity must not be forwarded to upstream services by default.

---

## Key Flows

- F1. Direct browser access to a provider-protected route
  - **Trigger:** A requester opens a protected route without a valid Route Access Session.
  - **Actors:** Requester, gateway, Access Provider.
  - **Steps:** Gateway renders a protected-route page, redirects to the provider when the requester continues, consumes a valid Access Grant, sets a host-only Route Access Session, and proxies the original route.
  - **Outcome:** The requester enters only if the provider grants access for the current route protection.

- F2. Dashboard iframe access to a protected internal tool
  - **Trigger:** A dashboard embeds a protected route for a requester already known to the product surface.
  - **Actors:** Dashboard, gateway, Access Provider, requester.
  - **Steps:** Dashboard obtains a single-use Access Grant, iframe opens the gateway consume path on the route host, gateway creates a Route Access Session, and iframe loads the upstream service.
  - **Outcome:** The embedded tool appears without a manual sign-in click when the product surface can satisfy the current route protection.

- F3. Route protection changes while sessions exist
  - **Trigger:** A deploy or route-protection operation changes protection for a route.
  - **Actors:** Operator, gateway, requester, dashboard where applicable.
  - **Steps:** Existing sessions for the old route protection stop applying. Direct browser users re-enter through the protected-route page. Dashboard embeds may perform Embed Access Renewal against the current protection.
  - **Outcome:** Stale access fails closed while the dashboard can make valid renewals feel seamless.

- F4. Full deploy replaces route state
  - **Trigger:** Cloud or another caller submits a deploy spec with routes.
  - **Actors:** Caller, core, gateway.
  - **Steps:** Core treats the submitted route set as authoritative, detaches omitted routes, normalizes omitted protection to public, and replaces included route protection exactly.
  - **Outcome:** Deploy behavior is simple and predictable, with no merge semantics for route protection.

---

## Acceptance Examples

- AE1. Given a current `drizzle.example.com` route protected by Access Provider `plz_cloud`, when a deploy spec omits that route, then the route binding is detached.
- AE2. Given a current protected route, when a deploy spec includes that route with no protection field, then the route becomes public in normalized core state.
- AE3. Given a valid Route Access Session for a protected route, when only the upstream service revision changes, then the session remains valid.
- AE4. Given a valid Route Access Session for a protected route, when the route protection changes, then the old session no longer admits requests.
- AE5. Given a dashboard iframe with a stale Access Grant, when the gateway rejects consume because provider or protection state changed, then the embed may retry once with fresh evidence for the current route protection.
- AE6. Given a protected route whose Access Provider has been disabled or removed from usable gateway state, when a requester opens the route, then the gateway denies access and does not proxy upstream.
- AE7. Given an API request to a protected route without valid access, when the gateway classifies the request as non-navigation, then it returns a status response rather than an HTML sign-in page.

---

## Scope Boundaries

**Deferred**

- Password protection implementation, until Ployz has a real secret-material story.
- Server-to-server provider refresh, device binding, route-level session TTL overrides, and per-user instant revocation.
- Path-specific protection on one hostname.
- Non-browser Access Provider flows for CLI/API clients.
- Upstream identity headers or signed identity forwarding.
- User-visible route logout.

**Outside v1**

- Core interpreting Cloud orgs, projects, BetterAuth sessions, SSO groups, or product permissions.
- Route protection securing internal/container-local network reachability.
- Product-facing `cloud` presets in core APIs or stored core state.

---

## Dependencies And Assumptions

- The route-binding model follows the existing attach-readiness posture from `docs/adr/0002-route-bindings-require-active-certificates.md`.
- The generic Access Provider boundary is recorded in `docs/adr/0012-access-provider-route-protection-stays-generic-in-core.md`.
- The gateway can reserve same-origin paths under `/.ployz/*` on route hosts.
- Gateway replicas can share or receive Gateway Session Key material through local secret authority.
- Product surfaces that want seamless iframe renewal can observe or refresh their own product state, but gateway consume remains authoritative against core route state.

---

## Sources

- `CONTEXT.md`
- `docs/adr/0002-route-bindings-require-active-certificates.md`
- `docs/adr/0012-access-provider-route-protection-stays-generic-in-core.md`
- `crates/ployzd/src/gateway.rs`
- `crates/ployzd/src/gateway_runtime.rs`
- `crates/ployz-core/src/state.rs`
