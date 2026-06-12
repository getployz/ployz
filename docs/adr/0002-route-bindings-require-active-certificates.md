# Route Bindings Require Active Certificates

Ployz route bindings require an active certificate before they can be attached or served. A deploy or route operation that needs a domain must ensure the certificate first; if certificate readiness fails, the route binding is not created and the operation reports that failure.

This deliberately rejects the Uncloud/Caddy-style model where a route can appear attached while certificate provisioning may still be pending or silently broken. In Ployz, "route attached" should mean the gateway has enough certificate material to serve TLS now.
