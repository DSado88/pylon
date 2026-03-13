use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBuffer, MTLCommandQueue, MTLDevice, MTLFunction, MTLLibrary, MTLPixelFormat,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLResourceOptions,
    MTLCreateSystemDefaultDevice,
};

use crate::error::{CockpitError, Result};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Uniforms {
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub atlas_width: f32,
    pub atlas_height: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub x_origin: f32,
    pub y_origin: f32,
}

// C4 fix: Layout matches Metal's packed struct exactly. All fields are scalar
// or tightly-packed float arrays — no float4 alignment gaps. The Metal shader
// uses packed_float4 to match this layout (sizeof = 56 bytes).
//
// I8 fix: atlas_uv_x/y/w/h carry per-cell atlas coordinates so the vertex
// shader can compute correct texture UVs from glyph_index.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GpuCell {
    pub glyph_index: u32,
    pub fg_color: [f32; 4],
    pub bg_color: [f32; 4],
    pub flags: u32,
    pub atlas_uv_x: f32,
    pub atlas_uv_y: f32,
    pub atlas_uv_w: f32,
    pub atlas_uv_h: f32,
}

impl Default for GpuCell {
    fn default() -> Self {
        Self {
            glyph_index: 0,
            fg_color: [1.0, 1.0, 1.0, 1.0],
            bg_color: [0.0, 0.0, 0.0, 1.0],
            flags: 0,
            atlas_uv_x: 0.0,
            atlas_uv_y: 0.0,
            atlas_uv_w: 0.0,
            atlas_uv_h: 0.0,
        }
    }
}

pub struct MetalContext {
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pub render_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub cell_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    pub uniforms_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    pub cell_capacity: usize,
    // Sidebar GPU resources
    pub sidebar_cell_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    pub sidebar_uniforms_buffer: Retained<ProtocolObject<dyn MTLBuffer>>,
    pub sidebar_cell_capacity: usize,
}

impl MetalContext {
    pub fn new(grid_cols: u32, grid_rows: u32) -> Result<Self> {
        let device: Retained<ProtocolObject<dyn MTLDevice>> = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| CockpitError::Metal("no Metal device found".into()))?;

        let command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>> = device
            .newCommandQueue()
            .ok_or_else(|| CockpitError::Metal("failed to create command queue".into()))?;

        let metallib_bytes: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cell.metallib"));

        let library: Retained<ProtocolObject<dyn MTLLibrary>> = if metallib_bytes.is_empty() {
            // Offline compilation was unavailable; compile from source at runtime
            let source = NSString::from_str(include_str!("shaders/cell.metal"));
            device
                .newLibraryWithSource_options_error(&source, None)
                .map_err(|e| CockpitError::Metal(format!("runtime shader compile failed: {e}")))?
        } else {
            let dispatch_data = dispatch2::DispatchData::from_bytes(metallib_bytes);
            device
                .newLibraryWithData_error(&dispatch_data)
                .map_err(|e| CockpitError::Metal(format!("failed to load metallib: {e}")))?
        };

        let vertex_name = NSString::from_str("cell_vertex");
        let fragment_name = NSString::from_str("cell_fragment");

        let vertex_fn: Retained<ProtocolObject<dyn MTLFunction>> = library
            .newFunctionWithName(&vertex_name)
            .ok_or_else(|| CockpitError::Metal("cell_vertex function not found".into()))?;

        let fragment_fn: Retained<ProtocolObject<dyn MTLFunction>> = library
            .newFunctionWithName(&fragment_name)
            .ok_or_else(|| CockpitError::Metal("cell_fragment function not found".into()))?;

        let pipeline_desc = MTLRenderPipelineDescriptor::new();
        pipeline_desc.setVertexFunction(Some(&vertex_fn));
        pipeline_desc.setFragmentFunction(Some(&fragment_fn));

        let color_attachments = pipeline_desc.colorAttachments();
        let color0 = unsafe { color_attachments.objectAtIndexedSubscript(0) };
        color0.setPixelFormat(MTLPixelFormat::BGRA8Unorm);

        let render_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>> = device
            .newRenderPipelineStateWithDescriptor_error(&pipeline_desc)
            .map_err(|e| CockpitError::Metal(format!("pipeline state creation failed: {e}")))?;

        let cell_capacity = (grid_cols as usize) * (grid_rows as usize);
        let cell_buffer = Self::alloc_cell_buffer(&device, cell_capacity)?;

        let uniforms_buffer: Retained<ProtocolObject<dyn MTLBuffer>> = device
            .newBufferWithLength_options(
                std::mem::size_of::<Uniforms>(),
                MTLResourceOptions::StorageModeShared,
            )
            .ok_or_else(|| CockpitError::Metal("failed to create uniforms buffer".into()))?;

        // Sidebar buffers — start with a small default
        let sidebar_default_capacity = 40 * 40;
        let sidebar_cell_buffer = Self::alloc_cell_buffer(&device, sidebar_default_capacity)?;
        let sidebar_uniforms_buffer: Retained<ProtocolObject<dyn MTLBuffer>> = device
            .newBufferWithLength_options(
                std::mem::size_of::<Uniforms>(),
                MTLResourceOptions::StorageModeShared,
            )
            .ok_or_else(|| CockpitError::Metal("failed to create sidebar uniforms buffer".into()))?;

        Ok(Self {
            device,
            command_queue,
            render_pipeline,
            cell_buffer,
            uniforms_buffer,
            cell_capacity,
            sidebar_cell_buffer,
            sidebar_uniforms_buffer,
            sidebar_cell_capacity: sidebar_default_capacity,
        })
    }

    /// Reallocate the cell buffer if the grid has grown beyond current capacity.
    pub fn resize_if_needed(&mut self, grid_cols: u32, grid_rows: u32) -> Result<()> {
        let needed = (grid_cols as usize) * (grid_rows as usize);
        if needed <= self.cell_capacity {
            return Ok(());
        }
        self.cell_buffer = Self::alloc_cell_buffer(&self.device, needed)?;
        self.cell_capacity = needed;
        Ok(())
    }

    /// Reallocate the sidebar cell buffer if needed.
    pub fn resize_sidebar_if_needed(&mut self, sidebar_cols: u32, sidebar_rows: u32) -> Result<()> {
        let needed = (sidebar_cols as usize) * (sidebar_rows as usize);
        if needed <= self.sidebar_cell_capacity {
            return Ok(());
        }
        self.sidebar_cell_buffer = Self::alloc_cell_buffer(&self.device, needed)?;
        self.sidebar_cell_capacity = needed;
        Ok(())
    }

    fn alloc_cell_buffer(
        device: &ProtocolObject<dyn MTLDevice>,
        cell_count: usize,
    ) -> Result<Retained<ProtocolObject<dyn MTLBuffer>>> {
        let size = cell_count * std::mem::size_of::<GpuCell>();
        device
            .newBufferWithLength_options(size, MTLResourceOptions::StorageModeShared)
            .ok_or_else(|| CockpitError::Metal("failed to create cell buffer".into()))
    }
}
