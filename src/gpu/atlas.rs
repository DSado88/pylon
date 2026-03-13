use std::collections::HashMap;
use std::ptr::NonNull;

use core_graphics::base::kCGImageAlphaNone;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGSize};
use core_text::font::new_from_name as ct_new_from_name;
use core_text::font::CTFont;
use core_text::font_descriptor::kCTFontOrientationHorizontal;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage,
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
    font: CTFont,
    font_bold: CTFont,
    font_italic: CTFont,
    pub cell_width: f32,
    pub cell_height: f32,
    raster_width: usize,
    raster_height: usize,
    ascent: f64,
}

impl GlyphAtlas {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        font_family: &str,
        font_size: f32,
    ) -> Result<Self> {
        let font = ct_new_from_name(font_family, font_size as f64)
            .map_err(|()| CockpitError::Glyph(format!("font not found: {font_family}")))?;

        // Derive bold and italic variants
        let bold_trait = core_text::font_descriptor::kCTFontBoldTrait;
        let italic_trait = core_text::font_descriptor::kCTFontItalicTrait;

        let font_bold = font
            .clone_with_symbolic_traits(bold_trait, bold_trait)
            .unwrap_or_else(|| font.clone_with_font_size(font_size as f64));

        let font_italic = font
            .clone_with_symbolic_traits(italic_trait, italic_trait)
            .unwrap_or_else(|| font.clone_with_font_size(font_size as f64));

        // Compute cell dimensions from font metrics
        let ascent = font.ascent();
        let descent = font.descent();
        let leading = font.leading();
        let cell_height = (ascent + descent + leading).ceil();

        // Cell width from advance of '0' (monospace representative)
        let cell_width = Self::measure_advance(&font, '0');

        let raster_width = cell_width.ceil() as usize;
        let raster_height = cell_height.ceil() as usize;

        // Create atlas texture
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

        let mut atlas = Self {
            allocator,
            texture,
            cache: HashMap::new(),
            font,
            font_bold,
            font_italic,
            cell_width: cell_width as f32,
            cell_height: cell_height as f32,
            raster_width,
            raster_height,
            ascent,
        };

        // Pre-rasterize printable ASCII (0x20-0x7E) for fast startup
        for ch in ' '..='~' {
            let key = GlyphKey {
                ch,
                bold: false,
                italic: false,
            };
            let _ = atlas.get_or_insert(key);
        }

        Ok(atlas)
    }

    fn measure_advance(font: &CTFont, ch: char) -> f64 {
        let mut utf16 = [0u16; 2];
        ch.encode_utf16(&mut utf16);
        let mut glyph = 0u16;
        unsafe {
            font.get_glyphs_for_characters(utf16.as_ptr(), &mut glyph, 1);
            let mut advance = CGSize::new(0.0, 0.0);
            font.get_advances_for_glyphs(
                kCTFontOrientationHorizontal,
                &glyph,
                &mut advance,
                1,
            );
            advance.width.ceil()
        }
    }

    fn select_font(&self, key: &GlyphKey) -> &CTFont {
        if key.bold {
            &self.font_bold
        } else if key.italic {
            &self.font_italic
        } else {
            &self.font
        }
    }

    fn rasterize_glyph(&self, key: &GlyphKey) -> Vec<u8> {
        let w = self.raster_width;
        let h = self.raster_height;

        if key.ch == ' ' || key.ch == '\0' {
            return vec![0u8; w * h];
        }

        let font = self.select_font(key);

        // Convert char to glyph ID
        let mut utf16 = [0u16; 2];
        key.ch.encode_utf16(&mut utf16);
        let mut glyph = 0u16;
        let found = unsafe { font.get_glyphs_for_characters(utf16.as_ptr(), &mut glyph, 1) };

        if !found || glyph == 0 {
            return vec![0u8; w * h];
        }

        let color_space = CGColorSpace::create_device_gray();
        // Let CG allocate its own buffer (None) so we can read it back with data()
        let mut ctx = CGContext::create_bitmap_context(
            None,
            w,
            h,
            8,
            0, // let CG compute bytes_per_row
            &color_space,
            kCGImageAlphaNone,
        );

        ctx.set_allows_antialiasing(true);
        ctx.set_should_antialias(true);
        ctx.set_allows_font_smoothing(true);
        ctx.set_should_smooth_fonts(false); // no subpixel — single-channel atlas

        // White glyph on black background (R8Unorm = grayscale intensity)
        ctx.set_gray_fill_color(1.0, 1.0);

        // Set CGFont + size on the context for show_glyphs_at_positions
        let cg_font = font.copy_to_CGFont();
        ctx.set_font(&cg_font);
        ctx.set_font_size(font.pt_size());

        // Draw at baseline. CG origin is bottom-left, Y goes up.
        // Baseline sits at descent pixels above the bottom.
        let descent = font.descent();
        let position = CGPoint::new(0.0, descent);
        ctx.show_glyphs_at_positions(&[glyph], &[position]);

        // CG's data() returns rows top-to-bottom (matching Metal's top-left origin).
        // Just copy, accounting for CG's potentially larger bytes_per_row stride.
        let bpr = ctx.bytes_per_row();
        let data = ctx.data();
        let mut pixels = vec![0u8; w * h];
        for row in 0..h {
            let src_start = row * bpr;
            let dst_start = row * w;
            for col in 0..w {
                if let (Some(&src), Some(dst)) =
                    (data.get(src_start + col), pixels.get_mut(dst_start + col))
                {
                    *dst = src;
                }
            }
        }

        pixels
    }

    fn upload_to_texture(&self, pixels: &[u8], x: u16, y: u16, w: u16, h: u16) {
        let region = MTLRegion {
            origin: MTLOrigin {
                x: x as usize,
                y: y as usize,
                z: 0,
            },
            size: MTLSize {
                width: w as usize,
                height: h as usize,
                depth: 1,
            },
        };

        // SAFETY: pixels slice is valid for w*h bytes, region is within atlas bounds
        // (guaranteed by etagere allocator), and texture is StorageModeShared.
        unsafe {
            self.texture
                .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    region,
                    0,
                    NonNull::new_unchecked(pixels.as_ptr() as *mut _),
                    w as usize,
                );
        }
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

        let glyph_w = self.raster_width as i32;
        let glyph_h = self.raster_height as i32;

        let alloc = self
            .allocator
            .allocate(etagere::size2(glyph_w, glyph_h))
            .ok_or_else(|| CockpitError::Glyph("atlas full".into()))?;

        let atlas_x = alloc.rectangle.min.x as u16;
        let atlas_y = alloc.rectangle.min.y as u16;

        // Rasterize and upload
        let pixels = self.rasterize_glyph(&key);
        self.upload_to_texture(&pixels, atlas_x, atlas_y, glyph_w as u16, glyph_h as u16);

        let entry = GlyphEntry {
            x: atlas_x,
            y: atlas_y,
            width: glyph_w as u16,
            height: glyph_h as u16,
            bearing_x: 0,
            bearing_y: self.ascent.ceil() as i16,
        };

        self.cache.insert(key, entry);
        Ok(entry)
    }
}
