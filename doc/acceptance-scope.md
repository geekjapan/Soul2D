# Acceptance scope

This document defines compatibility obligations and their limits.
It is not a completed acceptance report.
Soul2D remains incomplete and is published as source without binary distribution.

## Compatibility target

The intended target is Cubism 5.3 Pro across the scoped feature set.
The [feature ledger](cubism-feature-ledger.md) tracks implementation work.
A feature family, parser, or sample collection is not an exhaustive inventory.

Native authoring, evaluated display, animation, persistence, and conversion
require separate evidence. Unsupported or unexecuted outcomes are not passes.
Features missing from available fixtures require dedicated probes.

The reference research and proposed versions are documented in
[cubism reference research](cubism-53-research.md).
Those choices do not establish that reference artifacts are installed,
licensed for a particular run, or verified.

## Exclusions

Scope exclusions must be explicit and documented rather than inferred from
missing implementation or fixtures.

- The Cubism external application API has a documented Soul2D mapping rather
  than a wire-compatible endpoint. Related separate vendor products have no
  promised counterpart. See [ADR 0002](adr/0002-cubism-external-api-mapping.md).
- Soul2D does not write `.moc3` or `.model3.json`, or derive `.pose3.json`
  and `.exp3.json` from a CMO3. Existing runtime metadata exports cover
  `.cdi3.json` and `.physics3.json`. Vendor acceptance remains separate.
  Reading runtime assets does not change these writer exclusions.
  See [ADR 0004](adr/0004-no-moc3-writer.md).

## Format boundaries

| Direction | Acceptance obligation |
| --- | --- |
| CMO3 and CAN3 back to their source formats | Persist supported edits and preserve unrelated source and opaque container data. |
| INX to INX | Preserve supported model and animation semantics, images, and unknown data. |
| CMO3 with linked CAN3 to INX | Preserve the declared hierarchy, geometry, parameters, and animation mapping. |
| INX to CMO3 with CAN3 | Define representable mappings and pass the required authoring and runtime comparisons. |

The conversion rows are obligations, not declarations that complete converters
exist. See the [conversion map](format-conversion-map.md) for unresolved mappings.

CAN3 is a separate animation project and requires an explicit model association.
A stored source path does not authorize automatic discovery or attachment.
Unknown fields and INX-native features require preservation, a defined mapping,
or an explicit refusal. Silent semantic loss is not successful conversion.

## Comparison requirements

A vendor acceptance run requires the following fixed inputs:

- Exact Editor and Core versions, artifact hashes, and legitimate environments.
- Input identities, reference provenance, and reproducible commands.
- Parameter poses, animation times, edited cases, and deterministic reset rules.
- Coordinate spaces, timing units, render settings, and image conventions.
- Numerical tolerances, discrete equality rules, and failure aggregation.
- Save/reopen checks and observations through the shared GUI and protocol state.

The [comparison contract](vendor-comparison-contract.md) contains proposed
criteria. Proposed criteria and historical results do not establish a current
vendor verdict. Criteria must be settled before the corresponding comparison.

## Current acceptance boundary

The source contains bounded native editing, evaluation, playback, rendering,
and automation implementations. Complete feature coverage, conversion mappings,
vendor comparisons, and desktop workflows still require evidence.

Private fixtures, local receipts, and past test counts are not public
reproduction prerequisites. The [development goal](development-goal.md)
remains broader than individual implemented operations.
