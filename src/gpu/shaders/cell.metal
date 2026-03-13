#include <metal_stdlib>
using namespace metal;

struct Uniforms {
    uint grid_cols;
    uint grid_rows;
    float cell_width;
    float cell_height;
    float atlas_width;
    float atlas_height;
    float viewport_width;
    float viewport_height;
};

// C4 fix: Use packed_float4 so the struct layout matches Rust's [f32; 4]
// (tightly packed, no 16-byte alignment padding). sizeof(GpuCell) = 56 bytes
// in both Rust and Metal.
struct GpuCell {
    uint glyph_index;           // offset  0
    packed_float4 fg_color;     // offset  4
    packed_float4 bg_color;     // offset 20
    uint flags;                 // offset 36
    float atlas_uv_x;          // offset 40 — I8: atlas rect set by CPU
    float atlas_uv_y;          // offset 44
    float atlas_uv_w;          // offset 48
    float atlas_uv_h;          // offset 52
};                              // total  56

struct VertexOut {
    float4 position [[position]];
    float2 tex_coord;
    half4 fg_color;
    half4 bg_color;
};

vertex VertexOut cell_vertex(
    uint vertex_id [[vertex_id]],
    uint instance_id [[instance_id]],
    device const GpuCell* cells [[buffer(0)]],
    constant Uniforms& uniforms [[buffer(1)]]
) {
    uint col = instance_id % uniforms.grid_cols;
    uint row = instance_id / uniforms.grid_cols;

    // 6 vertices per quad (2 triangles): 0,1,2, 2,1,3
    float2 corners[4] = {
        float2(0.0, 0.0),    // top-left
        float2(1.0, 0.0),    // top-right
        float2(0.0, 1.0),    // bottom-left
        float2(1.0, 1.0),    // bottom-right
    };
    uint indices[6] = {0, 1, 2, 2, 1, 3};
    float2 corner = corners[indices[vertex_id]];

    // Convert to screen coordinates
    float x = (float(col) + corner.x) * uniforms.cell_width;
    float y = (float(row) + corner.y) * uniforms.cell_height;

    // Normalize to Metal clip space (-1 to 1)
    float2 ndc;
    ndc.x = (x / uniforms.viewport_width) * 2.0 - 1.0;
    ndc.y = 1.0 - (y / uniforms.viewport_height) * 2.0;  // flip Y

    // I8 fix: Compute atlas texture coordinates from per-cell UV rect
    device const GpuCell& cell = cells[instance_id];
    float2 uv;
    uv.x = cell.atlas_uv_x + corner.x * cell.atlas_uv_w;
    uv.y = cell.atlas_uv_y + corner.y * cell.atlas_uv_h;

    VertexOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.tex_coord = uv;
    out.fg_color = half4(float4(cell.fg_color));
    out.bg_color = half4(float4(cell.bg_color));
    return out;
}

fragment half4 cell_fragment(
    VertexOut in [[stage_in]],
    texture2d<half> atlas [[texture(0)]]
) {
    constexpr sampler s(filter::linear);
    half4 glyph_alpha = atlas.sample(s, in.tex_coord);
    return mix(in.bg_color, in.fg_color, glyph_alpha.r);
}
