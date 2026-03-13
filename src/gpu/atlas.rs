use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};

use crate::error::{CockpitError, Result};

const ATLAS_SIZE: u32 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub ch: char,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct GlyphEntry {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub bearing_x: i16,
    pub bearing_y: i16,
}

pub struct GlyphAtlas {
    allocator: etagere::BucketedAtlasAllocator,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    cache: HashMap<GlyphKey, GlyphEntry>,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl GlyphAtlas {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self> {
        // SAFETY: Valid pixel format and dimensions within GPU limits (2048x2048).
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::R8Unorm,
                ATLAS_SIZE as usize,
                ATLAS_SIZE as usize,
                false,
            )
        };
        desc.setStorageMode(MTLStorageMode::Shared);
        desc.setUsage(MTLTextureUsage::ShaderRead);

        let texture: Retained<ProtocolObject<dyn MTLTexture>> = device
            .newTextureWithDescriptor(&desc)
            .ok_or_else(|| CockpitError::Glyph("failed to create atlas texture".into()))?;

        let allocator = etagere::BucketedAtlasAllocator::new(etagere::size2(
            ATLAS_SIZE as i32,
            ATLAS_SIZE as i32,
        ));

        Ok(Self {
            allocator,
            texture,
            cache: HashMap::new(),
            cell_width: 8.0,
            cell_height: 17.0,
        })
    }

    pub fn texture(&self) -> &ProtocolObject<dyn MTLTexture> {
        &self.texture
    }

    pub fn atlas_width(&self) -> f32 {
        ATLAS_SIZE as f32
    }

    pub fn atlas_height(&self) -> f32 {
        ATLAS_SIZE as f32
    }

    pub fn get_or_insert(&mut self, key: GlyphKey) -> Result<GlyphEntry> {
        if let Some(entry) = self.cache.get(&key) {
            return Ok(*entry);
        }

        // Placeholder: allocate a rectangle but don't rasterize yet.
        // Real CoreText rasterization will be added in a later pass.
        let glyph_w = self.cell_width as i32;
        let glyph_h = self.cell_height as i32;

        let alloc = self
            .allocator
            .allocate(etagere::size2(glyph_w, glyph_h))
            .ok_or_else(|| CockpitError::Glyph("atlas full".into()))?;

        let entry = GlyphEntry {
            x: alloc.rectangle.min.x as u16,
            y: alloc.rectangle.min.y as u16,
            width: glyph_w as u16,
            height: glyph_h as u16,
            bearing_x: 0,
            bearing_y: self.cell_height as i16,
        };

        self.cache.insert(key, entry);
        Ok(entry)
    }
}
