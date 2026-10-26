# Official sample corpus

The verification scope is every free sample on
<https://www.live2d.com/learn/sample/>, including each published locale archive
and every bundled model, animation and runtime variant. The paid nizima ranking
is a separate marketplace. Do not replace corpus coverage with the four initial
CMO3 fixtures or infer identical contents from matching character names.

`tools/inventory-official-samples.py` reads a saved copy of the official
`https://cubism.live2d.com/sample-data/js/download.js` manifest. It records the
manifest hash, sample and locale identities, exact archive URLs, and local paths.
Archives belong under `<archive-root>/<sample>/<sample>_<locale>.zip`.

```sh
python3 tools/inventory-official-samples.py --self-test
python3 tools/inventory-official-samples.py \
  --script build/verification/native-resume-20261011/sample-download.js \
  --archives build/verification/official-sample-archives \
  --output build/verification/native-resume-20261011/official-sample-inventory.json
```

The tool neither downloads nor extracts files. For each available ZIP it reads
every member, validates CRCs through the ZIP reader, and records uncompressed
size, extension and SHA-256. Duplicate and unsafe member paths are rejected.
Missing or invalid archives remain explicit. A structurally changed upstream
manifest is rejected rather than silently producing a partial catalog.

`inventoried_not_tested` means only that archive contents were read successfully.
Each member starts with `verification: not_run`. Actual acceptance requires
format-appropriate import, display, deformation, overlap, playback, editing,
Undo/Redo and save/reopen evidence, with Cubism reference comparisons where
applicable. Archive hashes can identify identical bytes, but do not establish
that either copy passed these checks.

Run source-projection verification separately after inventory:

```sh
python3 tools/verify-official-cmo-sources.py \
  build/verification/native-resume-20261011/official-sample-inventory.json \
  --output build/verification/native-resume-20261011/official-cmo-sources
```

This compares every inventoried CMO3 with the independent XML oracle and the
Rust JSONL reader. Identical member hashes share a check, while each archive
member retains its own result. A failure identifies native reading, XML oracle
reading, or the final comparison as its stage. Missing archives leave the run
incomplete; malformed archives or failed CMO3 checks fail it. CAN3 and runtime
files remain outside this particular check.

Older CMO3 files can store generated normal forms in `extendedKeyforms`, before
the separate `normalExtendedKeyforms` and `morphTargetExtendedKeyforms` lists.
The reader maps that historical list to the same normal-form response. It
requires both lists in the modern layout and rejects mixed layouts. Ownership,
form identity, numeric limits and grid-reference checks still apply.

`python3 tools/verify-legacy-extended.py path/to/rice.cmo3` checks the historical
file against an equivalent modern representation, rejects malformed variants,
and verifies byte-exact save and reopen. This verifies source handling, not the
visual accuracy of extended interpolation.

An owned form list can repeat references to the same resolved XML object. The
reader projects that object once. Distinct objects with the same GUID and
coordinate type still reject, and array counts and ownership remain checked.
The original reference layout remains intact in saved CMO3 bytes.

`python3 tools/verify-form-aliases.py path/to/kei_vowels_pro_t02.cmo3` compares
repeated, single and transitive references, rejects distinct identity collisions
and invalid counts, and verifies byte-exact save and reopen. These are source
checks; they do not establish deformation or Cubism visual parity.

Some official CAN3 parameter curves have an empty `optionParam` map. The source
projection preserves this omission as `sourceParameter: null`. Binding reports
use the `missing` status after verifying the target parameter's exact GUID and
attribute ID against the explicit CMO3 model. They do not infer omitted source
metadata. Nonempty maps without `KEY_PARAM_ID` and malformed maps still reject.

`python3 tools/verify-empty-can3-metadata.py animation.can3 model.cmo3` checks
source and binding oracles through FFI, JSONL and MCP, byte-exact save/reopen,
key editing with undo/redo and persistence, and malformed-map refusals. This
does not establish GUI behavior or Cubism playback parity.

To check every inventoried CAN3 source member, run:

```sh
python3 tools/verify-official-can3-sources.py \
  build/verification/native-resume-20261011/official-sample-inventory.json \
  --output build/verification/official-can3-compat-20261011/can3-sources
```

This compares the reader's supported source projection with the raw XML oracle.
It retains every archive/member result, including duplicate member hashes, and
reports missing archives as incomplete. It does not verify ignored track types,
native playback or vendor equivalence.

CAN3 detection, reading and key rewriting share a finite limit of 2,000,000 XML
nodes. This covers the largest inventoried official source at 1,166,846 nodes
and a scene that keys 100 parameters on 600 frames (1,620,099 nodes measured on
2026-10-20: 27.5 MB of XML, open 8.2 s, key edit 17.5 s, save 16.0 s, 3.2 GB of
peak memory on a debug build; the 64 MiB text cap alone would allow about
3,780,000 nodes at 7.2 GB).
DTD input still rejects. The 64 MiB XML, 256 MiB container, copied-text and scalar
budgets remain enforced. Passing this source check does not prove playback or
memory use on other operating systems.

Before acquiring samples, obtain the user's agreement to the linked material
license and each model's terms. Preserve original archives and operate on
scratch copies for editing checks. Acquisition does not authorize publication
or redistribution of the models or outputs.
