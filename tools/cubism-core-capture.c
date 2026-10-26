/* Reference-data collector, compiled against the supplied matching Core header. */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef CORE_CAPTURE_SELF_CHECK
#include <Live2DCubismCore.h>
#endif

static void fail(const char *message) {
    fprintf(stderr, "%s\n", message);
    exit(1);
}

static size_t count(int value, size_t maximum) {
    if (value < 0 || (size_t)value > maximum) fail("invalid Core array count");
    return (size_t)value;
}

static void *aligned_buffer(size_t size, size_t alignment) {
    if (!size || size > 512u * 1024u * 1024u) fail("invalid Core buffer size");
    void *buffer = aligned_alloc(alignment, (size + alignment - 1) / alignment * alignment);
    if (!buffer) fail("Core buffer allocation failed");
    return buffer;
}

static void string(const char *value) {
    if (!value) fail("null Core identifier");
    putchar('"');
    for (const unsigned char *p = (const unsigned char *)value; *p; ++p) {
        if (*p == '"' || *p == '\\') printf("\\%c", *p);
        else if (*p < 32) printf("\\u%04x", *p);
        else putchar(*p);
    }
    putchar('"');
}

static void number(float value) {
    if (!isfinite(value)) fail("nonfinite Core value");
    printf("%.9g", (double)value);
}

#ifdef CORE_CAPTURE_SELF_CHECK
int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "nan")) number(NAN);
    if (argc > 1 && !strcmp(argv[1], "count")) count(-1, 10);
    if (argc > 1 && !strcmp(argv[1], "size")) aligned_buffer(0, 64);
    for (size_t alignment = 16; alignment <= 64; alignment *= 4) {
        for (size_t size = 1; size <= 129; ++size) {
            void *p = aligned_buffer(size, alignment);
            if ((size_t)p % alignment) fail("misaligned buffer");
            memset(p, 0x5a, size);
            free(p);
        }
    }
    printf("{\"string\":"); string("quote\" slash\\ line\n tab\t");
    printf(",\"number\":"); number(0.375f);
    printf(",\"count\":%zu}\n", count(10, 10));
    return 0;
}
#else
static void integers(const int *values, size_t length) {
    if (length && !values) fail("null Core integer array");
    putchar('[');
    for (size_t i = 0; i < length; ++i) printf("%s%d", i ? "," : "", values[i]);
    putchar(']');
}

static void vectors(const csmVector2 *values, size_t length) {
    if (length && !values) fail("null Core vector array");
    putchar('[');
    for (size_t i = 0; i < length; ++i) {
        if (i) putchar(',');
        putchar('['); number(values[i].X); putchar(','); number(values[i].Y); putchar(']');
    }
    putchar(']');
}

/* Core keeps the geometry of an object whose parameter leaves its keyed span (Rice's ArtMesh120 stayed where
   ParamArmR02 had put it once ParamArmR02Y went below its first key), so every pose starts from a settled default
   pose: the change flags describe the transition from it. */
static void settle(csmModel *model, size_t parameters, float *values, const float *defaults) {
    for (size_t p = 0; p < parameters; ++p) values[p] = defaults[p];
    csmUpdateModel(model);
}

static void pose(csmModel *model, size_t drawables, size_t parameters,
                 const char *parameter, float value) {
    csmResetDrawableDynamicFlags(model);
    csmUpdateModel(model);
    const int *counts = csmGetDrawableVertexCounts(model);
    const csmVector2 **positions = csmGetDrawableVertexPositions(model);
    const float *opacities = csmGetDrawableOpacities(model);
    const int *orders = csmGetDrawableDrawOrders(model);
    const int *render_orders = csmGetRenderOrders(model);
    const unsigned char *flags = csmGetDrawableDynamicFlags(model);
    const float *values = csmGetParameterValues(model);
    if (!counts || !positions || !opacities || !orders || !render_orders || !flags || !values)
        fail("null Core pose arrays");
    printf("{\"kind\":\"pose\",\"parameterId\":");
    if (parameter) string(parameter); else printf("null");
    printf(",\"sample\":"); if (parameter) number(value); else printf("null");
    printf(",\"parameterValues\":[");
    for (size_t p = 0; p < parameters; ++p) { if (p) putchar(','); number(values[p]); }
    printf("],\"drawables\":[");
    size_t total = 0;
    for (size_t d = 0; d < drawables; ++d) {
        size_t n = count(counts[d], 2000000); total += n;
        if (total > 2000000) fail("Core vertex budget exceeded");
        if (d) putchar(',');
        printf("{\"positions\":"); vectors(positions[d], n);
        printf(",\"opacity\":"); number(opacities[d]);
        printf(",\"drawOrder\":%d,\"renderOrder\":%d,\"dynamicFlags\":%u}", orders[d], render_orders[d],
            (unsigned int)flags[d]);
    }
    printf("]}\n");
}

int main(int argc, char **argv) {
    if (argc != 2) fail("usage: cubism-core-capture original.moc3");
    FILE *file = fopen(argv[1], "rb");
    if (!file || fseek(file, 0, SEEK_END)) fail("cannot read MOC");
    long length = ftell(file);
    if (length < 4 || length > 128 * 1024 * 1024 || fseek(file, 0, SEEK_SET)) fail("invalid MOC size");
    void *moc_memory = aligned_buffer((size_t)length, 64);
    if (fread(moc_memory, 1, (size_t)length, file) != (size_t)length || fclose(file)) fail("MOC read failed");
    if (memcmp(moc_memory, "MOC3", 4)) fail("invalid MOC signature");
    unsigned int version = csmGetMocVersion(moc_memory, (unsigned int)length);
    if (!version || version > csmGetLatestMocVersion()) fail("unsupported MOC version");
    csmMoc *moc = csmReviveMocInPlace(moc_memory, (unsigned int)length);
    if (!moc) fail("Core rejected MOC");
    unsigned int model_size = csmGetSizeofModel(moc);
    void *model_memory = aligned_buffer(model_size, 16);
    csmModel *model = csmInitializeModelInPlace(moc, model_memory, model_size);
    if (!model) fail("Core model initialization failed");
    size_t parameters = count(csmGetParameterCount(model), 4096);
    size_t drawables = count(csmGetDrawableCount(model), 65536);
    if (!parameters || !drawables) fail("empty Core model");
    const char **ids = csmGetParameterIds(model);
    const float *defaults = csmGetParameterDefaultValues(model);
    const float *minimum = csmGetParameterMinimumValues(model);
    const float *maximum = csmGetParameterMaximumValues(model);
    /* The official API specifies that the current-values buffer is writable. */
    float *values = (float *)csmGetParameterValues(model);
    if (!ids || !defaults || !minimum || !maximum || !values) fail("null Core parameter arrays");
    for (size_t p = 0; p < parameters; ++p) {
        if (!isfinite(minimum[p]) || !isfinite(maximum[p]) || !isfinite(defaults[p]) ||
            minimum[p] > defaults[p] || defaults[p] > maximum[p]) fail("invalid Core parameter bounds");
        values[p] = defaults[p];
    }
    csmResetDrawableDynamicFlags(model);
    csmUpdateModel(model);
    csmVector2 canvas, origin;
    float pixels_per_unit;
    csmReadCanvasInfo(model, &canvas, &origin, &pixels_per_unit);
    if (canvas.X <= 0 || canvas.Y <= 0 || pixels_per_unit <= 0) fail("invalid Core canvas");
    printf("{\"kind\":\"metadata\",\"coreVersion\":%u,\"mocVersion\":%u,\"canvas\":",
        csmGetVersion(), version);
    vectors(&canvas, 1); printf(",\"canvasOrigin\":"); vectors(&origin, 1);
    printf(",\"pixelsPerUnit\":"); number(pixels_per_unit);
    printf(",\"parameters\":[");
    for (size_t p = 0; p < parameters; ++p) {
        if (p) putchar(',');
        printf("{\"id\":"); string(ids[p]); printf(",\"minimum\":"); number(minimum[p]);
        printf(",\"maximum\":"); number(maximum[p]); printf(",\"default\":"); number(defaults[p]); putchar('}');
    }
    const char **drawable_ids = csmGetDrawableIds(model);
    const int *vertex_counts = csmGetDrawableVertexCounts(model);
    const csmVector2 **uvs = csmGetDrawableVertexUvs(model);
    const int *index_counts = csmGetDrawableIndexCounts(model);
    const unsigned short **indices = csmGetDrawableIndices(model);
    const int *texture_indices = csmGetDrawableTextureIndices(model);
    const unsigned char *constant_flags = csmGetDrawableConstantFlags(model);
    const int *mask_counts = csmGetDrawableMaskCounts(model);
    const int **masks = csmGetDrawableMasks(model);
    if (!drawable_ids || !vertex_counts || !uvs || !index_counts || !indices ||
        !texture_indices || !constant_flags || !mask_counts || !masks) fail("null Core drawable arrays");
    printf("],\"drawables\":[");
    size_t total_vertices = 0, total_indices = 0, total_masks = 0;
    for (size_t d = 0; d < drawables; ++d) {
        size_t nv = count(vertex_counts[d], 2000000), ni = count(index_counts[d], 6000000);
        size_t nm = count(mask_counts[d], drawables);
        total_vertices += nv; total_indices += ni; total_masks += nm;
        if (total_vertices > 2000000 || total_indices > 6000000 || total_masks > 1000000)
            fail("Core drawable budget exceeded");
        if ((ni && !indices[d]) || (nm && !masks[d])) fail("null Core drawable row");
        if (d) putchar(',');
        printf("{\"id\":"); string(drawable_ids[d]); printf(",\"uvs\":"); vectors(uvs[d], nv);
        printf(",\"indices\":[");
        for (size_t i = 0; i < ni; ++i) {
            if (indices[d][i] >= nv) fail("invalid Core triangle index");
            printf("%s%u", i ? "," : "", (unsigned int)indices[d][i]);
        }
        printf("],\"textureIndex\":%d,\"constantFlags\":%u,\"masks\":", texture_indices[d],
            (unsigned int)constant_flags[d]);
        for (size_t i = 0; i < nm; ++i) if (masks[d][i] < 0 || (size_t)masks[d][i] >= drawables)
            fail("invalid Core mask index");
        integers(masks[d], nm); putchar('}');
    }
    printf("]}\n");
    pose(model, drawables, parameters, NULL, 0);
    for (size_t p = 0; p < parameters; ++p) {
        const float edges[] = {minimum[p], maximum[p]};
        for (size_t edge = 0; edge < 2; ++edge) {
            if (edges[edge] == defaults[p]) continue;
            settle(model, parameters, values, defaults);
            values[p] = edges[edge];
            pose(model, drawables, parameters, ids[p], edges[edge]);
        }
    }
    /* Combined poses: every parameter at once, at a repeatable pseudo-random point of its range. The count is
       COMBINED_POSES in capture-cubism-core.py. */
    unsigned int seed = 12345u;
    for (size_t combined = 0; combined < 12; ++combined) {
        settle(model, parameters, values, defaults);
        for (size_t p = 0; p < parameters; ++p) {
            seed = seed * 1664525u + 1013904223u;
            values[p] = minimum[p] + (maximum[p] - minimum[p]) * ((float)(seed >> 8) / 16777216.0f);
        }
        pose(model, drawables, parameters, NULL, 0);
    }
    free(model_memory); free(moc_memory);
    if (fflush(stdout) || ferror(stdout)) fail("capture output failed");
    return 0;
}
#endif
