use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLDrawable,
    MTLLoadAction, MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLStoreAction,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use crate::error::{CockpitError, Result};

use super::atlas::GlyphAtlas;
use super::context::{GpuCell, MetalContext, Uniforms};

pub struct TerminalRenderer {
    pub ctx: MetalContext,
    pub atlas: GlyphAtlas,
}

impl TerminalRenderer {
    pub fn new(
        grid_cols: u32,
        grid_rows: u32,
        font_family: &str,
        font_size: f32,
    ) -> Result<Self> {
        let ctx = MetalContext::new(grid_cols, grid_rows)?;
        let atlas = GlyphAtlas::new(&ctx.device, font_family, font_size, 1.2)?;
        Ok(Self { ctx, atlas })
    }

    pub fn resize(&mut self, grid_cols: u32, grid_rows: u32) -> Result<()> {
        self.ctx.resize_if_needed(grid_cols, grid_rows)
    }

    pub fn resize_sidebar(&mut self, sidebar_cols: u32, sidebar_rows: u32) -> Result<()> {
        self.ctx.resize_sidebar_if_needed(sidebar_cols, sidebar_rows)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_uniforms(
        buffer: &ProtocolObject<dyn MTLBuffer>,
        cols: u32,
        rows: u32,
        atlas: &GlyphAtlas,
        viewport_w: f32,
        viewport_h: f32,
        x_origin: f32,
        y_origin: f32,
    ) {
        let uniforms = Uniforms {
            grid_cols: cols,
            grid_rows: rows,
            cell_width: atlas.cell_width,
            cell_height: atlas.cell_height,
            atlas_width: atlas.atlas_width(),
            atlas_height: atlas.atlas_height(),
            viewport_width: viewport_w,
            viewport_height: viewport_h,
            x_origin,
            y_origin,
        };
        let ptr = buffer.contents().as_ptr() as *mut Uniforms;
        unsafe {
            std::ptr::write(ptr, uniforms);
        }
    }

    pub fn update_uniforms(
        &self,
        cols: u32,
        rows: u32,
        viewport_w: f32,
        viewport_h: f32,
        x_origin: f32,
        y_origin: f32,
    ) {
        Self::write_uniforms(
            &self.ctx.uniforms_buffer,
            cols,
            rows,
            &self.atlas,
            viewport_w,
            viewport_h,
            x_origin,
            y_origin,
        );
    }

    pub fn update_sidebar_uniforms(
        &self,
        cols: u32,
        rows: u32,
        viewport_w: f32,
        viewport_h: f32,
        x_origin: f32,
        y_origin: f32,
    ) {
        Self::write_uniforms(
            &self.ctx.sidebar_uniforms_buffer,
            cols,
            rows,
            &self.atlas,
            viewport_w,
            viewport_h,
            x_origin,
            y_origin,
        );
    }

    pub fn cell_buffer_ptr(&self) -> *mut GpuCell {
        self.ctx.cell_buffer.contents().as_ptr() as *mut GpuCell
    }

    pub fn sidebar_cell_buffer_ptr(&self) -> *mut GpuCell {
        self.ctx.sidebar_cell_buffer.contents().as_ptr() as *mut GpuCell
    }

    /// Compute safe sidebar dimensions that fit within buffer capacity.
    /// Used as fallback when resize_sidebar fails.
    pub fn clamp_sidebar_dims(sb_cols: u32, sb_rows: u32, capacity: usize) -> (u32, u32) {
        if sb_cols == 0 {
            return (0, 0);
        }
        let max_rows = capacity / (sb_cols as usize);
        (sb_cols, sb_rows.min(max_rows as u32))
    }

    pub fn upload_sidebar_cells(&self, cells: &[GpuCell]) {
        let ptr = self.sidebar_cell_buffer_ptr();
        let cap = self.ctx.sidebar_cell_capacity;
        let count = cells.len().min(cap);
        unsafe {
            std::ptr::copy_nonoverlapping(cells.as_ptr(), ptr, count);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &self,
        layer: &CAMetalLayer,
        grid_cols: u32,
        grid_rows: u32,
        bg_color: [f64; 4],
        sidebar_cols: u32,
        sidebar_rows: u32,
        draw_sidebar: bool,
    ) -> Result<()> {
        let drawable: objc2::rc::Retained<ProtocolObject<dyn CAMetalDrawable>> = layer
            .nextDrawable()
            .ok_or_else(|| CockpitError::Render("no drawable available".into()))?;

        let command_buffer: objc2::rc::Retained<ProtocolObject<dyn MTLCommandBuffer>> = self
            .ctx
            .command_queue
            .commandBuffer()
            .ok_or_else(|| CockpitError::Render("failed to create command buffer".into()))?;

        let render_pass_desc = MTLRenderPassDescriptor::renderPassDescriptor();
        let color_attachments = render_pass_desc.colorAttachments();
        let color0 = unsafe { color_attachments.objectAtIndexedSubscript(0) };

        let drawable_texture = drawable.texture();
        color0.setTexture(Some(&drawable_texture));
        color0.setLoadAction(MTLLoadAction::Clear);
        color0.setClearColor(MTLClearColor {
            red: bg_color[0],
            green: bg_color[1],
            blue: bg_color[2],
            alpha: bg_color[3],
        });
        color0.setStoreAction(MTLStoreAction::Store);

        let encoder: objc2::rc::Retained<ProtocolObject<dyn MTLRenderCommandEncoder>> =
            command_buffer
                .renderCommandEncoderWithDescriptor(&render_pass_desc)
                .ok_or_else(|| {
                    CockpitError::Render("failed to create render encoder".into())
                })?;

        encoder.setRenderPipelineState(&self.ctx.render_pipeline);

        // 1. Draw terminal grid
        unsafe {
            encoder.setVertexBuffer_offset_atIndex(
                Some(&*self.ctx.cell_buffer as &ProtocolObject<dyn MTLBuffer>),
                0,
                0,
            );
            encoder.setVertexBuffer_offset_atIndex(
                Some(&*self.ctx.uniforms_buffer as &ProtocolObject<dyn MTLBuffer>),
                0,
                1,
            );
            encoder.setFragmentTexture_atIndex(Some(self.atlas.texture()), 0);
        }

        let instance_count = (grid_cols as usize) * (grid_rows as usize);
        unsafe {
            encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                instance_count,
            );
        }

        // 2. Draw sidebar
        if draw_sidebar && sidebar_cols > 0 && sidebar_rows > 0 {
            unsafe {
                encoder.setVertexBuffer_offset_atIndex(
                    Some(&*self.ctx.sidebar_cell_buffer as &ProtocolObject<dyn MTLBuffer>),
                    0,
                    0,
                );
                encoder.setVertexBuffer_offset_atIndex(
                    Some(&*self.ctx.sidebar_uniforms_buffer as &ProtocolObject<dyn MTLBuffer>),
                    0,
                    1,
                );
            }

            let sidebar_instances = (sidebar_cols as usize) * (sidebar_rows as usize);
            unsafe {
                encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    6,
                    sidebar_instances,
                );
            }
        }

        encoder.endEncoding();

        let drawable_ref: &ProtocolObject<dyn MTLDrawable> =
            ProtocolObject::from_ref(&*drawable);
        command_buffer.presentDrawable(drawable_ref);
        command_buffer.commit();

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::TerminalRenderer;

    #[test]
    fn test_clamp_fits() {
        // 40 cols * 50 rows = 2000, capacity is 2000 — fits exactly
        let (c, r) = TerminalRenderer::clamp_sidebar_dims(40, 50, 2000);
        assert_eq!((c, r), (40, 50));

        // Smaller than capacity — unchanged
        let (c, r) = TerminalRenderer::clamp_sidebar_dims(10, 10, 2000);
        assert_eq!((c, r), (10, 10));
    }

    #[test]
    fn test_clamp_too_large() {
        // 40 cols * 60 rows = 2400, but capacity is only 2000
        // max_rows = 2000 / 40 = 50, so rows clamped to 50
        let (c, r) = TerminalRenderer::clamp_sidebar_dims(40, 60, 2000);
        assert_eq!((c, r), (40, 50));

        // 80 cols * 100 rows = 8000, capacity 1600
        // max_rows = 1600 / 80 = 20
        let (c, r) = TerminalRenderer::clamp_sidebar_dims(80, 100, 1600);
        assert_eq!((c, r), (80, 20));
    }

    #[test]
    fn test_clamp_zero_cols() {
        let (c, r) = TerminalRenderer::clamp_sidebar_dims(0, 50, 2000);
        assert_eq!((c, r), (0, 0));

        let (c, r) = TerminalRenderer::clamp_sidebar_dims(0, 0, 0);
        assert_eq!((c, r), (0, 0));
    }
}
