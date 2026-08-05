use super::*;

impl<'a> RenderContext<'a> {
    // === Drawing primitives ===

    /// Draw a filled rectangle.
    ///
    /// Coordinates are in pixels with (0, 0) at the top-left of the window.
    pub fn draw_rect(&mut self, left: f32, top: f32, right: f32, bottom: f32, color: Color) {
        self.state.renderer.push(DrawCommand::Rect {
            left,
            top,
            right,
            bottom,
            color,
        });
    }

    /// Draw a filled circle.
    pub fn draw_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Color) {
        let segments = default_circle_segments(radius);
        self.state.renderer.push(DrawCommand::Circle {
            cx,
            cy,
            radius,
            color,
            segments,
        });
    }

    /// Draw a line.
    pub fn draw_line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, color: Color) {
        self.state.renderer.push(DrawCommand::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            color,
        });
    }

    /// Draw a stroked circular arc (an annular band segment).
    ///
    /// The band is centered on `radius` and is `thickness` pixels wide,
    /// sweeping from `start_angle` to `end_angle` (radians). Pass
    /// `0.0..=2*PI` for a full ring. Segment count is chosen automatically
    /// from the radius and angular span.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        thickness: f32,
        color: Color,
    ) {
        let segments = default_arc_segments(radius, start_angle, end_angle);
        self.state.renderer.push(DrawCommand::Arc {
            cx,
            cy,
            radius,
            start_angle,
            end_angle,
            thickness,
            color,
            segments,
        });
    }

    /// Draw a line of text using the built-in 5×7 bitmap font.
    ///
    /// `(x, y)` is the top-left of the first glyph, in pixel coordinates.
    /// `scale` is the size of one font pixel in screen pixels (so each glyph is
    /// `5*scale` wide and `7*scale` tall). Text is drawn as filled rectangles
    /// through the flat-color pipeline — no texture upload, no font asset.
    /// Lowercase is rendered with the uppercase glyphs. Use
    /// [`text_width`](Self::text_width) to center or right-align.
    pub fn draw_text(&mut self, text: &str, x: f32, y: f32, scale: f32, color: Color) {
        use crate::drawing::font;
        let advance = (font::FONT_W + font::FONT_TRACKING) as f32 * scale;
        let mut cx = x;
        for ch in text.chars() {
            if ch != ' ' {
                let g = font::glyph(ch);
                for (row, cols) in g.iter().enumerate() {
                    for (col, &on) in cols.iter().enumerate() {
                        if on {
                            let px = cx + col as f32 * scale;
                            let py = y + row as f32 * scale;
                            self.draw_rect(px, py, px + scale, py + scale, color);
                        }
                    }
                }
            }
            cx += advance;
        }
    }

    /// Width in screen pixels that [`draw_text`](Self::draw_text) will occupy
    /// for `text` at the given `scale`.
    pub fn text_width(&self, text: &str, scale: f32) -> f32 {
        crate::drawing::font::text_width_px(text) as f32 * scale
    }

    /// Draw a texture at the specified rectangle.
    pub fn draw_texture(
        &mut self,
        texture: TextureHandle,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        self.state.renderer.push(DrawCommand::Texture {
            texture_id: texture.id,
            left,
            top,
            right,
            bottom,
        });
    }

    // === Texture management ===

    /// Load a texture from a file.
    pub fn load_image(&mut self, path: impl AsRef<Path>) -> Result<TextureHandle, VSEError> {
        Ok(self.state.renderer.load_image(path)?)
    }

    /// Create a texture from raw RGBA pixel data.
    pub fn load_texture_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<TextureHandle, VSEError> {
        Ok(self.state.renderer.load_texture_rgba(width, height, data)?)
    }

    /// Create a Gabor patch texture from parameters.
    pub fn create_gabor(&mut self, params: &GaborParams) -> Result<TextureHandle, VSEError> {
        let pixels = params.generate();
        Ok(self
            .state
            .renderer
            .load_texture_rgba(params.size, params.size, &pixels)?)
    }

    /// Unload a texture and free its GPU resources.
    pub fn unload_texture(&mut self, handle: TextureHandle) {
        self.state.renderer.unload_texture(handle);
    }

    // === Advanced stimuli ===

    /// Draw a sinusoidal or square-wave grating.
    ///
    /// The grating fills the rectangle defined by (left, top, right, bottom)
    /// in pixel coordinates. Parameters control spatial frequency, orientation,
    /// phase, contrast, and waveform type.
    pub fn draw_grating(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GratingParams,
    ) {
        self.state.renderer.push(DrawCommand::Grating {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
        });
    }

    /// Draw a Gabor patch (grating windowed by a Gaussian envelope).
    ///
    /// Unlike `create_gabor()` which generates a CPU texture, this computes
    /// the Gabor mathematically on the GPU each frame, allowing real-time
    /// parameter animation.
    pub fn draw_gabor_shader(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GaborParams,
    ) {
        self.state.renderer.push(DrawCommand::Gabor {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
            additive: false,
        });
    }

    /// Add a zero-centered Gabor modulation to the current framebuffer.
    ///
    /// This uses source-one/destination-one color blending, equivalent to
    /// Psychtoolbox's `Screen('BlendFunction', win, GL_ONE, GL_ONE)`. It is
    /// intended for fields of overlapping Gabors: positive and negative lobes
    /// sum linearly instead of each rectangular patch replacing the previous
    /// one. `params.background` is omitted because the framebuffer supplies the
    /// common background.
    pub fn draw_gabor_additive(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &GaborParams,
    ) {
        self.state.renderer.push(DrawCommand::Gabor {
            left,
            top,
            right,
            bottom,
            params: params.clone(),
            additive: true,
        });
    }

    /// Draw a noise pattern.
    ///
    /// Generates a noise texture on CPU from the given parameters and
    /// displays it in the specified rectangle. For animated noise, change
    /// `params.seed` each frame.
    ///
    /// Generated textures are cached by their parameters, so redrawing the same
    /// noise costs only a bind and a draw. Generating one is expensive — CPU
    /// noise synthesis, a GPU image allocation, and an upload that blocks until
    /// the GPU finishes — so a stimulus that changes every frame pays that on
    /// every frame. Prefer advancing `params.seed` on a slower schedule than
    /// the refresh rate, or pre-generating the sequence before the trial.
    ///
    /// The cache holds a bounded number of textures and evicts oldest-first, so
    /// long animated sequences do not grow without limit.
    pub fn draw_noise(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        params: &NoiseParams,
    ) -> Result<(), VSEError> {
        // Look up BEFORE generating: a hit skips the CPU synthesis too.
        let texture_id = match self.state.renderer.cached_noise_texture(params) {
            Some(id) => id,
            None => {
                let pixels = crate::drawing::noise::generate_noise(params);
                self.state.renderer.insert_noise_texture(params, &pixels)?
            }
        };
        self.state.renderer.push(DrawCommand::Noise {
            left,
            top,
            right,
            bottom,
            texture_id,
        });
        Ok(())
    }

    /// Draw filled circular dots at the specified positions.
    ///
    /// This is the rendering primitive for Random Dot Kinematograms.
    /// Positions are in pixel coordinates. Each dot is rendered as a
    /// filled circle with an anti-aliased edge.
    pub fn draw_dots(&mut self, positions: &[(f32, f32)], radius: f32, color: Color) {
        if positions.is_empty() {
            return;
        }
        self.state.renderer.push(DrawCommand::Dots {
            positions: positions.iter().map(|&(x, y)| [x, y]).collect(),
            radius,
            color,
        });
    }
}
