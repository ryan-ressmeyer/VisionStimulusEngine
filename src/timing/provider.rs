//! Timing provider trait and implementations.

use std::cell::RefCell;
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use ash::vk;
use tracing::warn;

use super::clock::{Clock, Timestamp};
use super::timing_source::TimingSource;
use crate::core::present_timing_ext as pt;

/// Abstracts timing backends for different Vulkan extensions.
pub trait TimingProvider {
    /// Which timing source this provider uses.
    fn source(&self) -> TimingSource;

    /// Get the display refresh cycle duration.
    /// Returns None if not yet known (e.g., still auto-detecting).
    fn refresh_cycle_duration(&self) -> Option<Duration>;

    /// Record the present time for the current frame.
    /// Called after the GPU fence signals.
    /// For CPU: returns clock.now().
    /// For GOOGLE: queries vkGetPastPresentationTimingGOOGLE.
    fn record_present_time(&self, clock: &Clock) -> Timestamp;

    /// Wait/schedule for a target present time.
    /// For CPU: spin-waits until target_time.
    /// For GOOGLE: target is passed to VkPresentTimeGOOGLE during present.
    fn wait_for_target(&self, target_time: Timestamp, clock: &Clock);

    /// Query the confirmed hardware scanout time for a specific frame number.
    ///
    /// Returns `Some(Timestamp)` when the driver has confirmed the scanout time for
    /// the frame identified by `frame_number`. Used by `run_buffered()` to attach
    /// hardware-verified timing to `FlipEvent::Presented`.
    ///
    /// The default implementation returns `None`; the CPU path uses fence-signal time
    /// from `record_present_time()` instead.
    fn confirmed_present_time_for(&self, _frame_number: u64, _clock: &Clock) -> Option<Timestamp> {
        None
    }

    /// Notify the provider that the swapchain was recreated and its handle changed.
    ///
    /// Providers that cache a swapchain handle (the EXT backend) **must** refresh it here —
    /// querying a retired swapchain handle is undefined behavior. Default: no-op.
    fn on_swapchain_recreated(&self, _swapchain: &Arc<vulkano::swapchain::Swapchain>) {}

    /// Sample the present-stage scanout clock against `CLOCK_MONOTONIC`, when the backend
    /// supports it. Default: `None` (CPU estimation has no hardware present clock to bridge).
    fn sample_present_calibration(&self) -> Option<CalibrationSample> {
        None
    }

    /// Read back confirmed per-present scanout timings from the driver, when the backend records
    /// them. Default: empty (CPU estimation has no hardware feedback). The EXT backend returns one
    /// record per present that carried `VkPresentTimingsInfoEXT`.
    fn query_scanouts(&self) -> Vec<pt::ScanoutFeedback> {
        Vec::new()
    }
}

/// Build the `vkGetCalibratedTimestampsKHR` sampler for a device, if the calibrated-timestamps
/// extension is enabled. Mirrors the loader pattern in `src/host/capture.rs`.
fn build_calibrated_timestamps_device(
    device: &Arc<vulkano::device::Device>,
) -> Option<ash::ext::calibrated_timestamps::Device> {
    use vulkano::VulkanObject;

    let enabled = device.enabled_extensions();
    if !(enabled.ext_calibrated_timestamps || enabled.khr_calibrated_timestamps) {
        return None;
    }

    let instance = device.instance();
    let ash_instance = unsafe {
        ash::Instance::load_with(
            |name| {
                std::mem::transmute(
                    instance
                        .library()
                        .get_instance_proc_addr(instance.handle(), name.as_ptr()),
                )
            },
            instance.handle(),
        )
    };
    let get_dpa = instance.fns().v1_0.get_device_proc_addr;
    let dev_handle = device.handle();
    let ash_device = unsafe {
        ash::Device::load_with(
            |name| std::mem::transmute(get_dpa(dev_handle, name.as_ptr())),
            dev_handle,
        )
    };
    Some(ash::ext::calibrated_timestamps::Device::new(
        &ash_instance,
        &ash_device,
    ))
}

/// CPU-based timing (fallback when no Vulkan timing extensions are available).
pub struct CpuTimingProvider {
    refresh_duration: std::cell::RefCell<Option<Duration>>,
}

impl Default for CpuTimingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuTimingProvider {
    pub fn new() -> Self {
        Self {
            refresh_duration: std::cell::RefCell::new(None),
        }
    }

    /// Set the auto-detected refresh duration.
    pub fn set_refresh_duration(&self, duration: Duration) {
        *self.refresh_duration.borrow_mut() = Some(duration);
    }
}

impl TimingProvider for CpuTimingProvider {
    fn source(&self) -> TimingSource {
        TimingSource::CpuEstimate
    }

    fn refresh_cycle_duration(&self) -> Option<Duration> {
        *self.refresh_duration.borrow()
    }

    fn record_present_time(&self, clock: &Clock) -> Timestamp {
        clock.now()
    }

    fn wait_for_target(&self, target_time: Timestamp, clock: &Clock) {
        while clock.now() < target_time {
            std::hint::spin_loop();
        }
    }
}

/// A single paired reading of the swapchain's `PRESENT_STAGE_LOCAL` scanout clock and the host
/// `CLOCK_MONOTONIC` clock, sampled as close together as the hardware allows.
///
/// `offset = mono_ns − stage_ns` bridges a scanout timestamp onto the host clock;
/// `max_deviation_ns` bounds how far apart the two reads actually were. Re-sampling over time
/// exposes relative clock drift (the slope of `mono_ns` vs `stage_ns`).
#[derive(Debug, Clone, Copy)]
pub struct CalibrationSample {
    /// Present-stage-local (scanout) clock, nanoseconds in the driver's opaque epoch.
    pub stage_ns: u64,
    /// Host `CLOCK_MONOTONIC`, nanoseconds.
    pub mono_ns: u64,
    /// Driver-reported bound on how far apart the two reads were, nanoseconds.
    pub max_deviation_ns: u64,
}

/// Provider using `VK_EXT_present_timing`.
///
/// Owns the loaded present-timing function pointers. It supplies the driver-reported
/// **refresh cycle duration**, sizes the driver's past-timing ring, and samples the display's
/// present-stage scanout clock ([`sample_present_calibration`](Self::sample_present_calibration)).
///
/// Hardware scanout timestamps live in the driver's opaque *present-stage-local* time domain
/// (`VK_TIME_DOMAIN_PRESENT_STAGE_LOCAL_EXT`, the only domain guaranteed to exist) — VSE's
/// **primary** experimental clock. It is bridged to the host `CLOCK_MONOTONIC` clock only on
/// demand, via the opt-in host-clock bridge built on `VK_KHR_calibrated_timestamps` (VSE does
/// **not** rely on a native `CLOCK_MONOTONIC` swapchain domain, which most drivers — including
/// Intel/ANV — do not expose). See `docs/clock-synchronization.md`. `record_present_time` still
/// returns CPU fence-signal time pending the raw-present feedback path; `timing_source` reports
/// `ExtPresentTiming`.
pub struct ExtPresentTimingProvider {
    fns: pt::PresentTimingFns,
    device: vk::Device,
    /// Holds the device alive for the calibrated-timestamps sampler's raw handle; consumed by
    /// the calibration subsystem's periodic re-sampling.
    #[allow(dead_code)]
    vk_device: Arc<vulkano::device::Device>,
    swapchain: RefCell<vk::SwapchainKHR>,
    cached_refresh: RefCell<Option<Duration>>,
    present_wait2: bool,
    /// `vkGetCalibratedTimestampsKHR` sampler, when `VK_KHR/EXT_calibrated_timestamps` is
    /// enabled. `None` disables present-stage calibration (falls back to CPU fence time).
    ct_device: Option<ash::ext::calibrated_timestamps::Device>,
    /// The `timeDomainId` for `PRESENT_STAGE_LOCAL` on the current swapchain, re-read on every
    /// swapchain recreation. `None` until probed (or if the domain is not offered).
    present_stage_domain_id: RefCell<Option<u64>>,
}

impl ExtPresentTimingProvider {
    /// Size of the driver's past-timing ring buffer (frames of history retained).
    const TIMING_QUEUE_SIZE: u32 = 16;
    /// Create the provider, loading fn pointers and configuring the swapchain's timing.
    ///
    /// Returns `None` if the extension function pointers cannot be loaded (in which case
    /// the caller should fall back to CPU timing).
    ///
    /// # Safety
    ///
    /// The device must have been created with `VK_EXT_present_timing` enabled (see
    /// [`crate::core::present_timing_ext::create_device_with_present_timing`]).
    pub unsafe fn new(
        device: &Arc<vulkano::device::Device>,
        swapchain: &Arc<vulkano::swapchain::Swapchain>,
        enabled: pt::EnabledPresentTimingFeatures,
    ) -> Option<Self> {
        use vulkano::VulkanObject;
        let fns = pt::PresentTimingFns::load(device, enabled.present_wait2)?;
        let ct_device = build_calibrated_timestamps_device(device);
        let provider = Self {
            fns,
            device: device.handle(),
            vk_device: device.clone(),
            swapchain: RefCell::new(swapchain.handle()),
            cached_refresh: RefCell::new(None),
            present_wait2: enabled.present_wait2,
            ct_device,
            present_stage_domain_id: RefCell::new(None),
        };
        provider.configure_swapchain();
        Some(provider)
    }

    /// Sample the swapchain's `PRESENT_STAGE_LOCAL` scanout clock and `CLOCK_MONOTONIC`
    /// together via `vkGetCalibratedTimestampsKHR`.
    ///
    /// Returns `None` if calibrated timestamps are unavailable or the present-stage domain id
    /// has not been probed for the current swapchain. Requesting `IMAGE_FIRST_PIXEL_OUT` as the
    /// present stage matches the scanout-begin timestamp the feedback path reports.
    pub fn sample_present_calibration(&self) -> Option<CalibrationSample> {
        use std::ffi::c_void;

        let ct = self.ct_device.as_ref()?;
        let domain_id = (*self.present_stage_domain_id.borrow())?;
        let sc = *self.swapchain.borrow();

        // The present-stage entry carries a VkSwapchainCalibratedTimestampInfoEXT in its pNext.
        let swap_info = pt::SwapchainCalibratedTimestampInfoEXT {
            s_type: pt::STYPE_SWAPCHAIN_CALIBRATED_TIMESTAMP_INFO_EXT,
            p_next: ptr::null(),
            swapchain: sc,
            present_stage: pt::PRESENT_STAGE_IMAGE_FIRST_PIXEL_OUT_BIT,
            time_domain_id: domain_id,
        };
        let mut stage_info = vk::CalibratedTimestampInfoEXT::default().time_domain(
            vk::TimeDomainEXT::from_raw(pt::TIME_DOMAIN_PRESENT_STAGE_LOCAL),
        );
        stage_info.p_next = &swap_info as *const _ as *const c_void;
        let mono_info = vk::CalibratedTimestampInfoEXT::default()
            .time_domain(vk::TimeDomainEXT::CLOCK_MONOTONIC);
        let infos = [stage_info, mono_info];

        match unsafe { ct.get_calibrated_timestamps(&infos) } {
            Ok((ts, max_deviation)) if ts.len() == 2 => Some(CalibrationSample {
                stage_ns: ts[0],
                mono_ns: ts[1],
                max_deviation_ns: max_deviation,
            }),
            Ok(_) => None,
            Err(e) => {
                warn!("vkGetCalibratedTimestampsKHR (present-stage) failed: {e:?}");
                None
            }
        }
    }

    /// Whether `VK_KHR_present_wait2` is available for pacing.
    pub fn has_present_wait2(&self) -> bool {
        self.present_wait2
    }

    /// (Re)enable the past-timing ring and re-select the time domain for the current
    /// swapchain. Called on creation and after every swapchain recreation.
    fn configure_swapchain(&self) {
        let sc = *self.swapchain.borrow();
        let r = unsafe { (self.fns.set_queue_size)(self.device, sc, Self::TIMING_QUEUE_SIZE) };
        if r != vk::Result::SUCCESS {
            warn!("vkSetSwapchainPresentTimingQueueSizeEXT failed: {r:?}");
        }
        self.log_offered_time_domains(sc);
        *self.cached_refresh.borrow_mut() = None;
    }

    /// Update the swapchain handle after recreation and reconfigure timing.
    pub fn update_swapchain(&self, swapchain: &Arc<vulkano::swapchain::Swapchain>) {
        use vulkano::VulkanObject;
        *self.swapchain.borrow_mut() = swapchain.handle();
        self.configure_swapchain();
    }

    /// Log the present-timing time domains the swapchain reports (diagnostic only).
    ///
    /// VSE does not depend on any particular domain being offered: present timestamps are
    /// bridged to the CPU clock through `VK_KHR_calibrated_timestamps`, not by matching a
    /// native `CLOCK_MONOTONIC` swapchain domain. This is purely to surface, in the log, what
    /// clock the driver reports present times in.
    fn log_offered_time_domains(&self, sc: vk::SwapchainKHR) {
        let mut props = pt::SwapchainTimeDomainPropertiesEXT {
            s_type: pt::STYPE_SWAPCHAIN_TIME_DOMAIN_PROPERTIES_EXT,
            p_next: ptr::null_mut(),
            time_domain_count: 0,
            p_time_domains: ptr::null_mut(),
            p_time_domain_ids: ptr::null_mut(),
        };
        let mut counter: u64 = 0;
        let _ = unsafe {
            (self.fns.get_time_domain_properties)(self.device, sc, &mut props, &mut counter)
        };
        let n = props.time_domain_count as usize;
        if n == 0 {
            return;
        }
        let mut domains = vec![0i32; n];
        let mut ids = vec![0u64; n];
        props.p_time_domains = domains.as_mut_ptr();
        props.p_time_domain_ids = ids.as_mut_ptr();
        let r = unsafe {
            (self.fns.get_time_domain_properties)(self.device, sc, &mut props, &mut counter)
        };
        if r == vk::Result::SUCCESS || r == vk::Result::INCOMPLETE {
            let count = props.time_domain_count as usize;
            tracing::debug!(
                "VK_EXT_present_timing time domains offered: {:?} \
                 (1000208000=PRESENT_STAGE_LOCAL, 1000208001=SWAPCHAIN_LOCAL, 1=CLOCK_MONOTONIC); \
                 bridged to the CPU clock via VK_KHR_calibrated_timestamps",
                &domains[..count]
            );
            // Record the PRESENT_STAGE_LOCAL domain id so the calibration sampler can name it.
            let stage_id = domains[..count]
                .iter()
                .zip(ids[..count].iter())
                .find(|(d, _)| **d == pt::TIME_DOMAIN_PRESENT_STAGE_LOCAL)
                .map(|(_, id)| *id);
            *self.present_stage_domain_id.borrow_mut() = stage_id;
        }
    }

    /// Read back confirmed scanout timings from the driver's past-timing ring via
    /// `vkGetPastPresentationTimingEXT`.
    ///
    /// Returns one [`ScanoutFeedback`](pt::ScanoutFeedback) per record the driver has ready,
    /// each carrying the `IMAGE_FIRST_PIXEL_OUT` scanout time (in the record's own time domain,
    /// normally `PRESENT_STAGE_LOCAL`) and the correlating `present_id`. **Empty** unless the
    /// matching present attached `VkPresentTimingsInfoEXT` (see [`PresentChain`]) — that is what
    /// tells the driver to record timing at all.
    ///
    /// **Destructive:** each record is *dequeued* from the driver's ring on read, so this must be
    /// called **at most once per frame** and its result cached — a second call the same frame
    /// returns nothing. `flip()` drains it once into `VSEState::recent_scanouts`.
    ///
    /// [`PresentChain`]: pt::PresentChain
    pub fn query_scanouts(&self) -> Vec<pt::ScanoutFeedback> {
        /// Fixed per-record stage-array capacity: only ~4 present stages are defined, so 8 always
        /// holds every reported stage in a single fill call (no nested two-call sizing needed).
        const STAGE_CAP: usize = 8;

        let sc = *self.swapchain.borrow();
        let info = pt::PastPresentationTimingInfoEXT {
            s_type: pt::STYPE_PAST_PRESENTATION_TIMING_INFO_EXT,
            p_next: ptr::null(),
            flags: 0,
            swapchain: sc,
        };

        // Call 1: null timings pointer → driver reports how many records are ready.
        let mut props = pt::PastPresentationTimingPropertiesEXT {
            s_type: pt::STYPE_PAST_PRESENTATION_TIMING_PROPERTIES_EXT,
            p_next: ptr::null_mut(),
            timing_properties_counter: 0,
            time_domains_counter: 0,
            presentation_timing_count: 0,
            p_presentation_timings: ptr::null_mut(),
        };
        let r = unsafe { (self.fns.get_past_presentation_timing)(self.device, &info, &mut props) };
        if r != vk::Result::SUCCESS && r != vk::Result::INCOMPLETE {
            warn!("vkGetPastPresentationTimingEXT (count) failed: {r:?}");
            return Vec::new();
        }
        let n = props.presentation_timing_count as usize;
        if n == 0 {
            return Vec::new();
        }

        // Per-record stage buffers (fixed capacity) + record array, wired together.
        let mut stage_bufs: Vec<[pt::PresentStageTimeEXT; STAGE_CAP]> =
            vec![[pt::PresentStageTimeEXT { stage: 0, time: 0 }; STAGE_CAP]; n];
        let mut records: Vec<pt::PastPresentationTimingEXT> = Vec::with_capacity(n);
        for buf in stage_bufs.iter_mut() {
            records.push(pt::PastPresentationTimingEXT {
                s_type: pt::STYPE_PAST_PRESENTATION_TIMING_EXT,
                p_next: ptr::null_mut(),
                present_id: 0,
                target_time: 0,
                present_stage_count: STAGE_CAP as u32,
                p_present_stages: buf.as_mut_ptr(),
                time_domain: 0,
                time_domain_id: 0,
                report_complete: 0,
            });
        }
        props.presentation_timing_count = n as u32;
        props.p_presentation_timings = records.as_mut_ptr();

        // Call 2: driver fills the records and their stage arrays. This **dequeues** them from
        // the driver's ring — each record is returned exactly once, so `query_scanouts` must be
        // called at most once per frame and its result cached (see `VSEState::recent_scanouts`).
        let r = unsafe { (self.fns.get_past_presentation_timing)(self.device, &info, &mut props) };
        if r != vk::Result::SUCCESS && r != vk::Result::INCOMPLETE {
            warn!("vkGetPastPresentationTimingEXT (fill) failed: {r:?}");
            return Vec::new();
        }
        let filled = (props.presentation_timing_count as usize).min(n);

        (0..filled)
            .map(|i| {
                let rec = &records[i];
                let count = (rec.present_stage_count as usize).min(STAGE_CAP);
                pt::feedback_from_record(rec, &stage_bufs[i][..count])
            })
            .collect()
    }

    /// Query the driver's refresh cycle duration for the current swapchain.
    fn query_refresh(&self) -> Option<Duration> {
        let sc = *self.swapchain.borrow();
        let mut props = pt::SwapchainTimingPropertiesEXT {
            s_type: pt::STYPE_SWAPCHAIN_TIMING_PROPERTIES_EXT,
            p_next: ptr::null_mut(),
            refresh_duration: 0,
            refresh_interval: 0,
        };
        let mut counter: u64 = 0;
        let r =
            unsafe { (self.fns.get_timing_properties)(self.device, sc, &mut props, &mut counter) };
        if r == vk::Result::SUCCESS && props.refresh_duration > 0 {
            Some(Duration::from_nanos(props.refresh_duration))
        } else {
            if r != vk::Result::SUCCESS {
                warn!("vkGetSwapchainTimingPropertiesEXT failed: {r:?}");
            }
            None
        }
    }
}

impl TimingProvider for ExtPresentTimingProvider {
    fn source(&self) -> TimingSource {
        TimingSource::ExtPresentTiming
    }

    fn refresh_cycle_duration(&self) -> Option<Duration> {
        if let Some(cached) = *self.cached_refresh.borrow() {
            return Some(cached);
        }
        if let Some(dur) = self.query_refresh() {
            *self.cached_refresh.borrow_mut() = Some(dur);
            Some(dur)
        } else {
            None
        }
    }

    fn record_present_time(&self, clock: &Clock) -> Timestamp {
        // Hardware scanout time requires calibrating the present-stage-local clock to the CPU
        // clock (VK_KHR_calibrated_timestamps) — a separate subsystem. Until then, use the
        // CPU fence-signal time, identical to the CPU-estimate path.
        clock.now()
    }

    fn wait_for_target(&self, target_time: Timestamp, clock: &Clock) {
        // Hardware scheduling rides on the raw present path (VkPresentTimingInfoEXT), which
        // needs the calibration subsystem to express the target in the driver's clock. Until
        // then, honor the target with a CPU spin so scheduled presents are not early.
        while clock.now() < target_time {
            std::hint::spin_loop();
        }
    }

    fn on_swapchain_recreated(&self, swapchain: &Arc<vulkano::swapchain::Swapchain>) {
        self.update_swapchain(swapchain);
    }

    fn sample_present_calibration(&self) -> Option<CalibrationSample> {
        ExtPresentTimingProvider::sample_present_calibration(self)
    }

    fn query_scanouts(&self) -> Vec<pt::ScanoutFeedback> {
        ExtPresentTimingProvider::query_scanouts(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_provider_source() {
        let provider = CpuTimingProvider::new();
        assert_eq!(provider.source(), TimingSource::CpuEstimate);
    }

    #[test]
    fn test_cpu_provider_refresh_duration() {
        let provider = CpuTimingProvider::new();
        assert!(provider.refresh_cycle_duration().is_none());
        provider.set_refresh_duration(Duration::from_micros(16_667));
        assert_eq!(
            provider.refresh_cycle_duration(),
            Some(Duration::from_micros(16_667))
        );
    }

    #[test]
    fn test_cpu_provider_record_present_time() {
        let provider = CpuTimingProvider::new();
        let clock = Clock::new();
        let t = provider.record_present_time(&clock);
        // Just verify it returns a valid timestamp
        assert!(t.as_micros() > 0 || t.as_micros() == 0);
    }

    #[test]
    fn test_cpu_provider_spin_wait() {
        let provider = CpuTimingProvider::new();
        let clock = Clock::new();
        let target = Timestamp::from_micros(clock.now().as_micros() + 1_000); // 1ms in future
        provider.wait_for_target(target, &clock);
        assert!(clock.now() >= target);
    }
}
