# Layered image import

PSD and KRA imports preserve editable layer pixels. Import reads the source; it does not rewrite the PSD or KRA.
The same pixel and size policy applies when planning or applying a layered-image merge.

## Layers outside the canvas

A KRA layer keeps pixels outside the document canvas. Its imported texture and position use the complete layer
bounds, so those pixels remain available when the Part is moved. The merged preview stored in a KRA ends at the
canvas and therefore does not define the editable layer's bounds.

This is the decision for #252: do not clip layers on import. Default-only tiles do not enlarge the bounds, but
non-default pixels outside the canvas do. Allocation and texture limits apply to the resulting complete bounds.
The synthetic `a_gap_between_the_canvas_and_a_tile_outside_it_takes_the_default_pixel` test checks both the KRA
pixels and the imported texture, including pixels beyond the canvas.

## A layer above the texture limit

Each texture must fit within 8192 pixels on each side and 16,777,216 pixels in total. A layer above either limit
is refused with its name, dimensions and the supported limit; the import does not silently omit the layer.
Container and aggregate memory limits are checked separately and can refuse a file first.

This is the decision for #254: retain refusal. Import does not rescale, clip or split a large layer into Parts.
Rescaling loses source detail; automatic tiling changes the model hierarchy and the correspondence between a
source layer and its merge target. Those require an explicit conversion feature rather than an implicit import
change. To import such artwork now, prepare smaller layers in the source application and keep the original file.

The synthetic PSD and KRA import-limit tests pin refusal and the diagnostic. Page-sized layers that exceed the
limit remain unsupported; this policy does not make the previously refused real-file corpus importable.
