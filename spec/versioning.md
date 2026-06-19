# CSP Versioning Policy

CSP is versioned `MAJOR.MINOR.PATCH` (the current version is exported as
`csp_core::CSP_VERSION`). Compatibility is negotiated through **capabilities**, not
through version sniffing — a client should look at `ServerCapabilities` to decide
what it can call, never at the version string.

## What each bump means

- **PATCH** — editorial spec fixes, schema regeneration with no shape change, bug
  fixes. No wire impact.
- **MINOR** — *additive, backward-compatible* changes: a new optional capability
  flag, a new method gated behind a new capability, a new optional field. Existing
  clients/servers keep working because they negotiate around what they don't know.
- **MAJOR** — *breaking* changes: removing or renaming a method, changing a field's
  type, repurposing an existing capability, or making a previously optional field
  required.

## Rules of thumb for evolving the protocol

1. **Add, don't mutate.** Prefer a new optional field or a new method over changing
   an existing one. New fields must be `Option`/defaulted so old peers omitting them
   stay valid.
2. **Gate every new behaviour behind a capability.** This is what lets a v1.2 client
   talk to a v1.0 server and vice versa. The reserved run-control methods (§8 of the
   spec) follow this: they exist in the namespace today but are unreachable until
   `runControl` is advertised `true`.
3. **Unknown is ignorable.** Receivers must ignore unknown fields and unknown
   notification methods rather than erroring. Unknown *request* methods return
   `MethodNotFound` (`-32601`).
4. **Schemas are generated.** `spec/schema/*.json` is produced from the `csp-core`
   types (`cargo run -p csp-core --example gen_schema`). Never hand-edit them; change
   the types and regenerate so the prose, schema, and implementation move together.
