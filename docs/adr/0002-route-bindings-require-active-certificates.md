# Route Bindings Require Active Certificates

Ployz route bindings require an active certificate before they can be served over TLS. A deploy or route operation that needs TLS must ensure the certificate first; if certificate readiness fails, TLS is not activated and the operation reports that failure.

The coreless v2 gateway initially ships HTTP ingress before unified certificate distribution (#822). During that slice, a Route Binding may be attached and served over HTTP without certificate material. "TLS active" still means the gateway has enough certificate material to serve TLS now — never active-but-pending.
