# Compose Diagnostics Are Strict By Default

Compose input diagnostics reject unsupported and unknown fields by default. Strict mode renders every finding sorted by path so an operator can fix the whole file in one pass, and it submits no deploy request when any `Unsupported`, `UnknownField`, or `InvalidValue` finding exists. Advisory findings are never fatal in either mode.

`--allow-unsupported` downgrades `Unsupported` and `UnknownField` findings to warnings and submits the supported subset. It never downgrades `InvalidValue`; malformed or semantically invalid input is always fatal.
