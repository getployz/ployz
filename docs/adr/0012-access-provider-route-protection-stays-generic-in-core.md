# Access Provider Route Protection Stays Generic In Core

Ployz core models protected routes through generic access providers instead of a built-in Cloud auth mode. Product surfaces such as Ployz Cloud may expose presets like "protect with Cloud", but those presets are resolved before core stores route protection or gateways enforce it.

Route protection is gateway-ingress state on a route binding. A provider-protected route may attach only when the referenced cluster-scoped access provider is usable, following ADR 0002's posture that an attached route should already have the material required for gateways to serve it correctly.

Gateways enforce provider-backed protection from projected route, provider, and session-key state, including last-known-good state when the control plane is unavailable. Access grants and route access sessions are route-binding scoped and fail closed when the route protection, access provider, or route binding identity no longer matches. Password protection remains a conceptual route protection mode, but it is deferred until Ployz has a real secret-material story.
