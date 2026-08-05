use super::*;

impl<'a> RenderContext<'a> {
    /// Capture a snapshot of the full host machine state.
    ///
    /// Returns a [`HostInfo`](crate::host::HostInfo) struct containing OS, CPU, memory, GPU,
    /// display, swapchain, pipeline config, build metadata, runtime
    /// environment, and EDID monitor data.
    ///
    /// This is an on-demand operation — call it when you need a snapshot.
    /// The EDID capture shells out to `xrandr`, which may take ~50ms.
    pub fn capture_host_info(&self) -> crate::host::HostInfo {
        capture_host_info(self.state, self.config)
    }
}

/// Snapshot the host machine and this session's render target.
///
/// Shared by [`RenderContext::capture_host_info`] and
/// [`HeadlessContext::capture_host_info`](crate::core::HeadlessContext::capture_host_info):
/// the headless context has no `RenderContext` to hand out when it is not
/// running a frame, but the snapshot does not need one.
pub(in crate::core) fn capture_host_info(
    state: &VSEState,
    config: &RenderConfig,
) -> crate::host::HostInfo {
    // Headless runs report the offscreen target in place of a swapchain,
    // and no present-timing observations — there was no presentation to
    // observe. The `SwapchainInfo` strings say so explicitly.
    let (render_target, observed) = match &state.target {
        RenderTarget::Present(p) => (
            crate::host::capture::capture_swapchain_info(&p.swapchain),
            crate::host::capture::ObservedPresentTiming {
                scanout_feedback_populated: p.scanout_feedback_populated,
                // Enforcement is not auto-probed (it disrupts frames); `None` unless a
                // characterization run recorded it via `record_absolute_scheduling_enforced`.
                absolute_scheduling_enforced: p.absolute_scheduling_enforced,
                present_timing_surface: p.swapchain.present_timing_surface_caps(),
                queue_global_priority: p.ext_features.map(|f| f.queue_priority),
            },
        ),
        RenderTarget::Offscreen(o) => (
            crate::host::capture::capture_offscreen_info(o.format, o.extent),
            crate::host::capture::ObservedPresentTiming::default(),
        ),
    };
    crate::host::capture::capture_host_info(
        state.device_selector.physical_device(),
        &state.device,
        state.target.present().and_then(|p| p.window.as_deref()),
        render_target,
        config,
        observed,
    )
}
