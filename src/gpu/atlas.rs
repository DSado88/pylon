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
        line_height_factor: f32,
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
        let cell_height = (ascent + descent + leading).ceil() * line_height_factor as f64;

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

        // Pre-warm bold ASCII to avoid first-use latency
        for ch in ' '..='~' {
            let key = GlyphKey {
                ch,
                bold: true,
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

    /// Draw box-drawing characters (U+2500–U+259F) and block elements as
    /// pixel-perfect geometric primitives. Returns Some(pixels) if handled,
    /// None to fall through to Core Text rasterization.
    fn rasterize_box_drawing(&self, ch: char) -> Option<Vec<u8>> {
        let code = ch as u32;
        if !(0x2500..=0x259F).contains(&code) {
            return None;
        }

        let w = self.raster_width;
        let h = self.raster_height;
        let mut pixels = vec![0u8; w * h];

        let cx = w / 2; // center x
        let cy = h / 2; // center y

        // Line thickness: thin = 1px, heavy = 2-3px depending on cell size
        let thin = 1usize.max(w / 10);
        let heavy = (thin * 2).max(2);

        // Helper closures for drawing lines
        let mut hline = |y_start: usize, y_end: usize, x_start: usize, x_end: usize| {
            for y in y_start..y_end.min(h) {
                for x in x_start..x_end.min(w) {
                    pixels[y * w + x] = 255;
                }
            }
        };

        // Decode the box-drawing character into segments:
        // Each char can have: left, right, up, down segments from center
        // with thin or heavy weight
        let half_thin = thin / 2;
        let half_heavy = heavy / 2;

        match code {
            // ─ light horizontal
            0x2500 => {
                hline(cy - half_thin, cy - half_thin + thin, 0, w);
            }
            // ━ heavy horizontal
            0x2501 => {
                hline(cy - half_heavy, cy - half_heavy + heavy, 0, w);
            }
            // │ light vertical
            0x2502 => {
                hline(0, h, cx - half_thin, cx - half_thin + thin);
            }
            // ┃ heavy vertical
            0x2503 => {
                hline(0, h, cx - half_heavy, cx - half_heavy + heavy);
            }
            // ╌ light triple dash horizontal (render as thin horizontal)
            0x254C => {
                hline(cy - half_thin, cy - half_thin + thin, 0, w);
            }
            // ╍ heavy triple dash horizontal
            0x254D => {
                hline(cy - half_heavy, cy - half_heavy + heavy, 0, w);
            }
            // ┌ light down and right
            0x250C => {
                hline(cy - half_thin, cy - half_thin + thin, cx, w); // right
                hline(cy, h, cx - half_thin, cx - half_thin + thin); // down
            }
            // ┐ light down and left
            0x2510 => {
                hline(cy - half_thin, cy - half_thin + thin, 0, cx + half_thin); // left
                hline(cy, h, cx - half_thin, cx - half_thin + thin); // down
            }
            // └ light up and right
            0x2514 => {
                hline(cy - half_thin, cy - half_thin + thin, cx, w); // right
                hline(0, cy + half_thin, cx - half_thin, cx - half_thin + thin); // up
            }
            // ┘ light up and left
            0x2518 => {
                hline(cy - half_thin, cy - half_thin + thin, 0, cx + half_thin); // left
                hline(0, cy + half_thin, cx - half_thin, cx - half_thin + thin); // up
            }
            // ├ light vertical and right
            0x251C => {
                hline(0, h, cx - half_thin, cx - half_thin + thin); // vertical
                hline(cy - half_thin, cy - half_thin + thin, cx, w); // right
            }
            // ┤ light vertical and left
            0x2524 => {
                hline(0, h, cx - half_thin, cx - half_thin + thin); // vertical
                hline(cy - half_thin, cy - half_thin + thin, 0, cx + half_thin); // left
            }
            // ┬ light down and horizontal
            0x252C => {
                hline(cy - half_thin, cy - half_thin + thin, 0, w); // horizontal
                hline(cy, h, cx - half_thin, cx - half_thin + thin); // down
            }
            // ┴ light up and horizontal
            0x2534 => {
                hline(cy - half_thin, cy - half_thin + thin, 0, w); // horizontal
                hline(0, cy + half_thin, cx - half_thin, cx - half_thin + thin); // up
            }
            // ┼ light cross
            0x253C => {
                hline(cy - half_thin, cy - half_thin + thin, 0, w); // horizontal
                hline(0, h, cx - half_thin, cx - half_thin + thin); // vertical
            }
            // ═ double horizontal
            0x2550 => {
                let gap = thin.max(1);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, w);
            }
            // ║ double vertical
            0x2551 => {
                let gap = thin.max(1);
                hline(0, h, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(0, h, cx + gap - half_thin, cx + gap - half_thin + thin);
            }
            // ╔ double down and right
            0x2554 => {
                let gap = thin.max(1);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, cx, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, cx + gap, w);
                hline(cy - gap, h, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(cy + gap, h, cx + gap - half_thin, cx + gap - half_thin + thin);
            }
            // ╗ double down and left
            0x2557 => {
                let gap = thin.max(1);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, cx + gap + half_thin);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(cy - gap, h, cx + gap - half_thin, cx + gap - half_thin + thin);
                hline(cy + gap, h, cx - gap - half_thin, cx - gap - half_thin + thin);
            }
            // ╚ double up and right
            0x255A => {
                let gap = thin.max(1);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, cx + gap, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, cx, w);
                hline(0, cy - gap + half_thin, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(0, cy + gap + half_thin, cx + gap - half_thin, cx + gap - half_thin + thin);
            }
            // ╝ double up and left
            0x255D => {
                let gap = thin.max(1);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, cx + gap + half_thin);
                hline(0, cy - gap + half_thin, cx + gap - half_thin, cx + gap - half_thin + thin);
                hline(0, cy + gap + half_thin, cx - gap - half_thin, cx - gap - half_thin + thin);
            }
            // ╠ double vertical and right
            0x2560 => {
                let gap = thin.max(1);
                hline(0, h, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(0, h, cx + gap - half_thin, cx + gap - half_thin + thin);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, cx + gap, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, cx + gap, w);
            }
            // ╣ double vertical and left
            0x2563 => {
                let gap = thin.max(1);
                hline(0, h, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(0, h, cx + gap - half_thin, cx + gap - half_thin + thin);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, cx - gap + half_thin);
            }
            // ╦ double down and horizontal
            0x2566 => {
                let gap = thin.max(1);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, cx + gap, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(cy + gap, h, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(cy + gap, h, cx + gap - half_thin, cx + gap - half_thin + thin);
            }
            // ╩ double up and horizontal
            0x2569 => {
                let gap = thin.max(1);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, w);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, cx + gap, w);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(0, cy - gap + half_thin, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(0, cy - gap + half_thin, cx + gap - half_thin, cx + gap - half_thin + thin);
            }
            // ╬ double cross
            0x256C => {
                let gap = thin.max(1);
                // Horizontal lines
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(cy - gap - half_thin, cy - gap - half_thin + thin, cx + gap, w);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, 0, cx - gap + half_thin);
                hline(cy + gap - half_thin, cy + gap - half_thin + thin, cx + gap, w);
                // Vertical lines
                hline(0, cy - gap + half_thin, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(0, cy - gap + half_thin, cx + gap - half_thin, cx + gap - half_thin + thin);
                hline(cy + gap, h, cx - gap - half_thin, cx - gap - half_thin + thin);
                hline(cy + gap, h, cx + gap - half_thin, cx + gap - half_thin + thin);
            }
            // █ full block
            0x2588 => {
                hline(0, h, 0, w);
            }
            // ▌ left half block
            0x258C => {
                hline(0, h, 0, cx);
            }
            // ▐ right half block
            0x2590 => {
                hline(0, h, cx, w);
            }
            // ▀ upper half block
            0x2580 => {
                hline(0, cy, 0, w);
            }
            // ▄ lower half block
            0x2584 => {
                hline(cy, h, 0, w);
            }
            // ░ light shade
            0x2591 => {
                for y in 0..h {
                    for x in 0..w {
                        if (x + y) % 4 == 0 {
                            pixels[y * w + x] = 255;
                        }
                    }
                }
            }
            // ▒ medium shade
            0x2592 => {
                for y in 0..h {
                    for x in 0..w {
                        if (x + y) % 2 == 0 {
                            pixels[y * w + x] = 255;
                        }
                    }
                }
            }
            // ▓ dark shade
            0x2593 => {
                for y in 0..h {
                    for x in 0..w {
                        if (x + y) % 4 != 0 {
                            pixels[y * w + x] = 255;
                        }
                    }
                }
            }
            // ▔ upper 1/8 block
            0x2594 => {
                let eighth = h / 8;
                hline(0, eighth.max(1), 0, w);
            }
            // ▕ right 1/8 block
            0x2595 => {
                let eighth = w / 8;
                hline(0, h, w.saturating_sub(eighth.max(1)), w);
            }
            // ▖ quadrant lower left
            0x2596 => {
                hline(cy, h, 0, cx);
            }
            // ▗ quadrant lower right
            0x2597 => {
                hline(cy, h, cx, w);
            }
            // ▘ quadrant upper left
            0x2598 => {
                hline(0, cy, 0, cx);
            }
            // ▙ quadrant upper left + lower left + lower right
            0x2599 => {
                hline(0, cy, 0, cx);
                hline(cy, h, 0, w);
            }
            // ▚ quadrant upper left + lower right
            0x259A => {
                hline(0, cy, 0, cx);
                hline(cy, h, cx, w);
            }
            // ▛ quadrant upper left + upper right + lower left
            0x259B => {
                hline(0, cy, 0, w);
                hline(cy, h, 0, cx);
            }
            // ▜ quadrant upper left + upper right + lower right
            0x259C => {
                hline(0, cy, 0, w);
                hline(cy, h, cx, w);
            }
            // ▝ quadrant upper right
            0x259D => {
                hline(0, cy, cx, w);
            }
            // ▞ quadrant upper right + lower left
            0x259E => {
                hline(0, cy, cx, w);
                hline(cy, h, 0, cx);
            }
            // ▟ quadrant upper right + lower left + lower right
            0x259F => {
                hline(0, cy, cx, w);
                hline(cy, h, 0, w);
            }
            // ▁ lower 1/8 block
            0x2581 => {
                let eighth = h / 8;
                hline(h.saturating_sub(eighth.max(1)), h, 0, w);
            }
            // ▂ lower 1/4 block
            0x2582 => {
                hline(h * 3 / 4, h, 0, w);
            }
            // ▃ lower 3/8 block
            0x2583 => {
                hline(h * 5 / 8, h, 0, w);
            }
            // ▅ lower 5/8 block
            0x2585 => {
                hline(h * 3 / 8, h, 0, w);
            }
            // ▆ lower 3/4 block
            0x2586 => {
                hline(h / 4, h, 0, w);
            }
            // ▇ lower 7/8 block
            0x2587 => {
                let eighth = h / 8;
                hline(eighth.max(1), h, 0, w);
            }
            // ▉ left 7/8 block
            0x2589 => {
                let eighth = w / 8;
                hline(0, h, 0, w.saturating_sub(eighth.max(1)));
            }
            // ▊ left 3/4 block
            0x258A => {
                hline(0, h, 0, w * 3 / 4);
            }
            // ▋ left 5/8 block
            0x258B => {
                hline(0, h, 0, w * 5 / 8);
            }
            // ▍ left 3/8 block
            0x258D => {
                hline(0, h, 0, w * 3 / 8);
            }
            // ▎ left 1/4 block
            0x258E => {
                hline(0, h, 0, w / 4);
            }
            // ▏ left 1/8 block
            0x258F => {
                hline(0, h, 0, (w / 8).max(1));
            }
            // ╭ rounded down and right (render as corner — at small sizes indistinguishable)
            0x256D => {
                hline(cy - half_thin, cy - half_thin + thin, cx, w); // right
                hline(cy, h, cx - half_thin, cx - half_thin + thin); // down
            }
            // ╮ rounded down and left
            0x256E => {
                hline(cy - half_thin, cy - half_thin + thin, 0, cx + half_thin); // left
                hline(cy, h, cx - half_thin, cx - half_thin + thin); // down
            }
            // ╯ rounded up and left
            0x256F => {
                hline(cy - half_thin, cy - half_thin + thin, 0, cx + half_thin); // left
                hline(0, cy + half_thin, cx - half_thin, cx - half_thin + thin); // up
            }
            // ╰ rounded up and right
            0x2570 => {
                hline(cy - half_thin, cy - half_thin + thin, cx, w); // right
                hline(0, cy + half_thin, cx - half_thin, cx - half_thin + thin); // up
            }
            // ╴ light left
            0x2574 => {
                hline(cy - half_thin, cy - half_thin + thin, 0, cx + half_thin);
            }
            // ╵ light up
            0x2575 => {
                hline(0, cy + half_thin, cx - half_thin, cx - half_thin + thin);
            }
            // ╶ light right
            0x2576 => {
                hline(cy - half_thin, cy - half_thin + thin, cx, w);
            }
            // ╷ light down
            0x2577 => {
                hline(cy, h, cx - half_thin, cx - half_thin + thin);
            }
            // ╸ heavy left
            0x2578 => {
                hline(cy - half_heavy, cy - half_heavy + heavy, 0, cx + half_heavy);
            }
            // ╹ heavy up
            0x2579 => {
                hline(0, cy + half_heavy, cx - half_heavy, cx - half_heavy + heavy);
            }
            // ╺ heavy right
            0x257A => {
                hline(cy - half_heavy, cy - half_heavy + heavy, cx, w);
            }
            // ╻ heavy down
            0x257B => {
                hline(cy, h, cx - half_heavy, cx - half_heavy + heavy);
            }
            // ╼ light left and heavy right
            0x257C => {
                hline(cy - half_thin, cy - half_thin + thin, 0, cx);
                hline(cy - half_heavy, cy - half_heavy + heavy, cx, w);
            }
            // ╽ light up and heavy down
            0x257D => {
                hline(0, cy, cx - half_thin, cx - half_thin + thin);
                hline(cy, h, cx - half_heavy, cx - half_heavy + heavy);
            }
            // ╾ heavy left and light right
            0x257E => {
                hline(cy - half_heavy, cy - half_heavy + heavy, 0, cx);
                hline(cy - half_thin, cy - half_thin + thin, cx, w);
            }
            // ╿ heavy up and light down
            0x257F => {
                hline(0, cy, cx - half_heavy, cx - half_heavy + heavy);
                hline(cy, h, cx - half_thin, cx - half_thin + thin);
            }
            // For any unhandled character in range, fall through to Core Text
            _ => return None,
        }

        Some(pixels)
    }

    fn rasterize_glyph(&self, key: &GlyphKey) -> Vec<u8> {
        let w = self.raster_width;
        let h = self.raster_height;

        if key.ch == ' ' || key.ch == '\0' {
            return vec![0u8; w * h];
        }

        // Try procedural box-drawing first — pixel-perfect, no gaps
        if let Some(pixels) = self.rasterize_box_drawing(key.ch) {
            return pixels;
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
        ctx.set_should_smooth_fonts(true); // weight boosting for thicker, cleaner glyphs

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

        // Allocate with 1px padding on each side to prevent bilinear
        // filtering from bleeding adjacent atlas entries.
        let pad = 1_i32;
        let alloc = self
            .allocator
            .allocate(etagere::size2(glyph_w + pad * 2, glyph_h + pad * 2))
            .ok_or_else(|| CockpitError::Glyph("atlas full".into()))?;

        // Upload glyph into the center of the padded region.
        // The padding stays zero (black) from the initial texture clear.
        let atlas_x = (alloc.rectangle.min.x + pad) as u16;
        let atlas_y = (alloc.rectangle.min.y + pad) as u16;

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
