# ADR 0002: Document a mapping for the Cubism external application API instead of implementing it

- **Status:** Accepted on 2026-10-12 by the maintainer's explicit decision, as `doc/acceptance-scope.md` requires for a scope exclusion
- **Date:** 2026-10-12
- **Context:** Issue #97 (ledger row API01) asks whether Soul2D should offer an endpoint compatible with the Cubism Editor external application integration API (WebSocket with JSON, default port 22033, token plus per-client user approval). Soul2D already serves one Session through JSONL, stdio MCP, HTTP MCP and ACP ([ADR 0001](0001-rust-authoritative-core.md)).

## Decision

Do **not** implement a wire-compatible endpoint. Document, for each vendor method and event, the Soul2D MCP tool or JSONL operation that covers it, and state where none exists. Cubism Viewer (OW and Unity), the Photoshop plug-in and scripts, the After Effects plug-in and the verification tool are separate products and have no Soul2D counterpart. A second protocol surface would need its own authentication and permission model, its conformance cannot be tested without a vendor Editor, and no client that needs it is known.

The mapping, the vendor facts with sources, and the conditions for revisiting are in [cubism-external-api.md](../cubism-external-api.md). Of 20 vendor methods and events, 0 are Equivalent, 11 Partial and 9 None.
