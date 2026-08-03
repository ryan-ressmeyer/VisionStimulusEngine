//! Headless (offscreen) rendering: the same renderer, no display.
//!
//! A headless session renders through the identical [`Renderer`] code path a
//! windowed session uses, into an offscreen image that is copied back to host
//! memory after every flip. Its purpose is post-hoc reproducibility: given a
//! recorded session's [`HostInfo`](crate::host::HostInfo) and the experiment's
//! render closure, regenerate the frames that were displayed.
//!
//! # What is and is not promised
//!
//! Frames regenerate identically on the **same machine and driver** as the
//! recording. Rasterization is not bit-guaranteed across GPU vendors or driver
//! versions, so a regeneration run on different hardware may differ in the low
//! bits. Match the recorded color format, extent, and
//! [`PipelineSuite`](crate::drawing::PipelineSuite) — rendering to
//! `R8G8B8A8_UNORM` what was displayed as `B8G8R8A8_SRGB` gives different
//! bytes even on identical hardware.
//!
//! # Timing
//!
//! There is no display, therefore no scanout clock and no measured
//! presentation. [`FlipInfo::present_time`](crate::timing::FlipInfo) is
//! synthesized as `frame_number × frame_interval` and tagged
//! [`TimingSource::Offscreen`], so a regenerated data file can never be
//! mistaken for a measured one.

use std::sync::Arc;
use std::time::Duration;

use tracing::info;
use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer};
use vulkano::format::Format;
use vulkano::image::{Image, ImageCreateInfo, ImageType, ImageUsage};
use vulkano::memory::allocator::{AllocationCreateInfo, MemoryTypeFilter};

use super::config::{VSEConfig, VSEError};
use super::device::DeviceSelector;
use super::input::InputState;
use super::render_context::RenderContext;
use super::state::{RenderTarget, VSEState};
use crate::data::ExperimentSession;
use crate::drawing::renderer::Renderer;
use crate::timing::Clock;

/// Default nominal refresh used to synthesize headless frame times when the
/// builder was given no expected refresh rate. 60 Hz, matching the fallback the
/// windowed missed-frame detector uses.
const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// One frame of rendered pixels, handed to the sink after every headless flip.
///
/// The bytes are the raw image contents in [`format`](Self::format) — no
/// conversion, no color-space change. Use [`pixel`](Self::pixel) to read a
/// single texel as RGBA8 regardless of the underlying channel order.
pub struct CapturedFrame {
    frame_number: u64,
    width: u32,
    height: u32,
    format: Format,
    bytes: Vec<u8>,
}

impl CapturedFrame {
    /// The frame number of the flip that produced this image.
    pub fn frame_number(&self) -> u64 {
        self.frame_number
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The image format these bytes are in — the same format the recorded
    /// session's swapchain used, when constructed to match it.
    pub fn format(&self) -> Format {
        self.format
    }

    /// The raw image bytes, row-major, in [`format`](Self::format).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Read one texel as RGBA8, normalizing BGRA channel order.
    ///
    /// Panics if `(x, y)` is outside the frame — an out-of-range index is a
    /// bug in the caller, not a runtime condition to handle.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) is outside a {}x{} frame",
            self.width,
            self.height
        );
        let i = ((y * self.width + x) * 4) as usize;
        let raw = [
            self.bytes[i],
            self.bytes[i + 1],
            self.bytes[i + 2],
            self.bytes[i + 3],
        ];
        if is_bgra(self.format) {
            [raw[2], raw[1], raw[0], raw[3]]
        } else {
            raw
        }
    }

    /// The whole frame as RGBA8, row-major — a copy, with BGRA normalized.
    pub fn to_rgba8(&self) -> Vec<u8> {
        if !is_bgra(self.format) {
            return self.bytes.clone();
        }
        let mut out = self.bytes.clone();
        for texel in out.chunks_exact_mut(4) {
            texel.swap(0, 2);
        }
        out
    }
}

/// Whether a 4-byte-per-texel format stores blue in the first byte.
fn is_bgra(format: Format) -> bool {
    matches!(
        format,
        Format::B8G8R8A8_UNORM | Format::B8G8R8A8_SRGB | Format::B8G8R8A8_SNORM
    )
}

/// Offscreen render target: one color image and its host-visible readback.
///
/// One image, not a ring: a headless flip blocks until the copy completes, so
/// there is nothing to overlap against.
pub(super) struct OffscreenTarget {
    pub(super) image: Arc<Image>,
    pub(super) format: Format,
    pub(super) extent: [u32; 2],
    pub(super) readback: Subbuffer<[u8]>,
    /// Nominal inter-frame interval used to synthesize flip timestamps.
    pub(super) frame_interval: Duration,
    /// Frames captured since the sink last drained them, in flip order. A
    /// render callback that flips more than once produces more than one.
    pub(super) captured: Vec<CapturedFrame>,
}

impl OffscreenTarget {
    /// Record a captured frame for the sink to drain after the callback returns.
    pub(super) fn push_capture(&mut self, frame_number: u64, bytes: Vec<u8>) {
        self.captured.push(CapturedFrame {
            frame_number,
            width: self.extent[0],
            height: self.extent[1],
            format: self.format,
            bytes,
        });
    }
}

/// A headless VSE session: GPU resources and an offscreen target, with no
/// window, swapchain, or event loop.
///
/// Unlike [`VSEContext`](super::context::VSEContext), whose GPU state cannot
/// exist until a window does, this is fully constructed by
/// [`VSEContextBuilder::build_headless`](super::config::VSEContextBuilder::build_headless)
/// — there is no surface to negotiate, so nothing has to wait for a run loop.
pub struct HeadlessContext {
    config: VSEConfig,
    state: VSEState,
}

impl HeadlessContext {
    /// Build the GPU core and offscreen target. Called by
    /// [`VSEContextBuilder::build_headless`](super::config::VSEContextBuilder::build_headless).
    pub(super) fn new(
        config: VSEConfig,
        session: Option<ExperimentSession>,
        format: Format,
        extent: [u32; 2],
    ) -> Result<Self, VSEError> {
        let device_selector = DeviceSelector::new(config.gpu_preference)?;
        let (device, queue) = device_selector.create_standard_device()?;

        let renderer = Renderer::new(
            device.clone(),
            queue.clone(),
            format,
            1,
            extent,
            &config.pipeline_suite,
        )?;

        let memory_allocator = renderer.memory_allocator();

        let image = Image::new(
            memory_allocator.clone(),
            ImageCreateInfo {
                image_type: ImageType::Dim2d,
                format,
                extent: [extent[0], extent[1], 1],
                // COLOR_ATTACHMENT to render into, TRANSFER_SRC for the readback
                // copy, TRANSFER_DST to mirror the swapchain's usage so the
                // renderer records against an identically-capable image.
                usage: ImageUsage::COLOR_ATTACHMENT
                    | ImageUsage::TRANSFER_SRC
                    | ImageUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo::default(),
        )
        .map_err(|e| VSEError::Window(format!("offscreen image creation failed: {e}")))?;

        let byte_len = (extent[0] as u64) * (extent[1] as u64) * 4;
        let readback = Buffer::new_slice::<u8>(
            memory_allocator,
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                ..Default::default()
            },
            byte_len,
        )
        .map_err(|e| VSEError::Window(format!("readback buffer allocation failed: {e}")))?;

        let frame_interval = config
            .expected_refresh_rate
            .map(|hz| Duration::from_micros((1_000_000.0 / hz) as u64))
            .unwrap_or(DEFAULT_FRAME_INTERVAL);

        info!(
            "Headless initialization complete: {}x{} {:?}, frame interval {} us",
            extent[0],
            extent[1],
            format,
            frame_interval.as_micros()
        );

        let recording = session.map(|sess| super::state::RecordingState {
            session: sess,
            pending_flip: None,
            last_claimed_frame: None,
        });

        Ok(Self {
            config,
            state: VSEState {
                device_selector,
                device,
                queue,
                renderer,
                clock: Clock::new(),
                frame_number: 0,
                should_close: false,
                input: InputState::new(),
                recording,
                target: RenderTarget::Offscreen(OffscreenTarget {
                    image,
                    format,
                    extent,
                    readback,
                    frame_interval,
                    captured: Vec::new(),
                }),
            },
        })
    }

    /// The color format frames are rendered in.
    pub fn format(&self) -> Format {
        self.offscreen().format
    }

    /// The offscreen target's size in pixels.
    pub fn extent(&self) -> [u32; 2] {
        self.offscreen().extent
    }

    /// Snapshot the host machine and this session's render target.
    ///
    /// The [`SwapchainInfo`](crate::host::SwapchainInfo) slot describes the
    /// offscreen target and says `n/a (headless)` where a presented session
    /// would report a color space and present mode — so a regeneration's
    /// metadata is never mistakable for a recording's.
    pub fn capture_host_info(&self) -> crate::host::HostInfo {
        super::render_context::capture_host_info(&self.state, &self.config)
    }

    fn offscreen(&self) -> &OffscreenTarget {
        match &self.state.target {
            RenderTarget::Offscreen(o) => o,
            RenderTarget::Present(_) => unreachable!("a headless context is never presenting"),
        }
    }

    /// Run the render loop until the callback calls
    /// [`request_exit`](RenderContext::request_exit).
    ///
    /// `render` is the experiment's per-frame closure — the same signature
    /// [`VSEContext::run`](super::context::VSEContext::run) takes, so a closure
    /// written for a windowed session runs here unchanged. `sink` receives every
    /// frame the closure flips, in order, immediately after the flip that
    /// produced it completes.
    ///
    /// Unlike the windowed loops, neither closure needs to be `'static`: there
    /// is no event loop to outlive, so both may borrow from the caller.
    ///
    /// ```no_run
    /// use vision_stimulus_engine::prelude::*;
    ///
    /// # fn demo() -> Result<(), VSEError> {
    /// let mut headless = VSEContext::builder()
    ///     .with_headless(256, 256)
    ///     .build_headless()?;
    ///
    /// let mut frames = 0;
    /// headless.run_headless(
    ///     |frame| {
    ///         println!("frame {} is {}x{}", frame.frame_number(), frame.width(), frame.height());
    ///         Ok(())
    ///     },
    ///     |vse| {
    ///         vse.draw_rect(0.0, 0.0, 64.0, 64.0, Color::WHITE);
    ///         vse.flip(None)?;
    ///         frames += 1;
    ///         if frames == 10 {
    ///             vse.request_exit();
    ///         }
    ///         Ok(())
    ///     },
    /// )
    /// # }
    /// ```
    pub fn run_headless<S, F>(&mut self, sink: S, mut render: F) -> Result<(), VSEError>
    where
        S: FnMut(&CapturedFrame) -> Result<(), VSEError>,
        F: FnMut(&mut RenderContext) -> Result<(), VSEError>,
    {
        self.run_headless_with_setup(|_| Ok(()), sink, move |ctx, _state: &mut ()| render(ctx))
    }

    /// Like [`run_headless`](Self::run_headless), but runs `setup` once before
    /// the first frame and threads its result into every frame — the headless
    /// counterpart of
    /// [`run_with_setup`](super::context::VSEContext::run_with_setup).
    ///
    /// Registering a [`StimulusPipeline`](crate::drawing::StimulusPipeline) or
    /// loading assets belongs here, exactly as it does windowed.
    pub fn run_headless_with_setup<Set, T, S, F>(
        &mut self,
        setup: Set,
        mut sink: S,
        mut render: F,
    ) -> Result<(), VSEError>
    where
        Set: FnOnce(&mut RenderContext) -> Result<T, VSEError>,
        S: FnMut(&CapturedFrame) -> Result<(), VSEError>,
        F: FnMut(&mut RenderContext, &mut T) -> Result<(), VSEError>,
    {
        let mut setup_state = {
            let mut ctx = RenderContext {
                state: &mut self.state,
                config: &mut self.config,
            };
            setup(&mut ctx)?
        };

        while !self.state.should_close {
            {
                let mut ctx = RenderContext {
                    state: &mut self.state,
                    config: &mut self.config,
                };
                render(&mut ctx, &mut setup_state)?;
            }

            // Hand every frame this iteration flipped to the sink, in order.
            let captured = match self.state.target.offscreen_mut() {
                Some(off) => std::mem::take(&mut off.captured),
                None => unreachable!("a headless context is never presenting"),
            };
            for frame in &captured {
                sink(frame)?;
            }

            self.state.input.begin_frame();
        }

        if let Some(recording) = &mut self.state.recording {
            recording.on_shutdown();
        }

        info!("HeadlessContext shut down cleanly");
        Ok(())
    }
}
