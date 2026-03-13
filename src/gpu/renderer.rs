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
    pub fn new(grid_cols: u32, grid_rows: u32) -> Result<Self> {
        let ctx = MetalContext::new(grid_cols, grid_rows)?;
        let atlas = GlyphAtlas::new(&ctx.device)?;
        Ok(Self { ctx, atlas })
    }

    pub fn resize(&mut self, grid_cols: u32, grid_rows: u32) -> Result<()> {
        self.ctx.resize_if_needed(grid_cols, grid_rows)
    }

    // I7: StorageModeShared on Apple Silicon UMA means CPU and GPU share the
    // same physical memory with coherent caches. No didModifyRange() call is
    // needed — that API is only required for StorageModeManaged (discrete GPU).
    // CPU writes are visible to the GPU once the command buffer is committed,
    // which acts as the synchronization point.
    pub fn update_uniforms(&self, cols: u32, rows: u32, viewport_w: f32, viewport_h: f32) {
        let uniforms = Uniforms {
            grid_cols: cols,
            grid_rows: rows,
            cell_width: self.atlas.cell_width,
            cell_height: self.atlas.cell_height,
            atlas_width: self.atlas.atlas_width(),
            atlas_height: self.atlas.atlas_height(),
            viewport_width: viewport_w,
            viewport_height: viewport_h,
        };

        // SAFETY: uniforms_buffer is StorageModeShared with size >= sizeof(Uniforms).
        let ptr = self.ctx.uniforms_buffer.contents().as_ptr() as *mut Uniforms;
        unsafe {
            std::ptr::write(ptr, uniforms);
        }
    }

    pub fn cell_buffer_ptr(&self) -> *mut GpuCell {
        self.ctx.cell_buffer.contents().as_ptr() as *mut GpuCell
    }

    pub fn render_frame(
        &self,
        layer: &CAMetalLayer,
        grid_cols: u32,
        grid_rows: u32,
        bg_color: [f64; 4],
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

        encoder.endEncoding();

        let drawable_ref: &ProtocolObject<dyn MTLDrawable> =
            ProtocolObject::from_ref(&*drawable);
        command_buffer.presentDrawable(drawable_ref);
        command_buffer.commit();

        Ok(())
    }
}
