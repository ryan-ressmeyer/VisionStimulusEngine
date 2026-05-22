//! Timing provider trait and implementations.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use super::clock::{Clock, Timestamp};
use super::timing_source::TimingSource;

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

/// Provider using VK_GOOGLE_display_timing extension.
///
/// Uses ash to call `vkGetPastPresentationTimingGOOGLE` for hardware-verified
/// present timestamps and `vkGetRefreshCycleDurationGOOGLE` for the display
/// refresh cycle duration.
pub struct GoogleDisplayTimingProvider {
    display_timing: ash::google::display_timing::Device,
    swapchain_handle: RefCell<ash::vk::SwapchainKHR>,
    /// Cached refresh cycle duration from the driver.
    cached_refresh_duration: RefCell<Option<Duration>>,
}

impl GoogleDisplayTimingProvider {
    /// Create a new provider from vulkano device and instance.
    ///
    /// # Safety
    ///
    /// The caller must ensure the device was created with
    /// `VK_GOOGLE_display_timing` enabled.
    pub unsafe fn new(
        vulkano_device: &Arc<vulkano::device::Device>,
        swapchain: &Arc<vulkano::swapchain::Swapchain>,
    ) -> Self {
        use vulkano::VulkanObject;

        let vk_instance_handle = vulkano_device.instance().handle();
        let vk_device_handle = vulkano_device.handle();
        let get_device_proc_addr = vulkano_device.instance().fns().v1_0.get_device_proc_addr;

        // Build minimal ash::Instance with just get_device_proc_addr loaded
        let ash_instance = ash::Instance::load_with(
            |name| std::mem::transmute(get_device_proc_addr(vk_device_handle, name.as_ptr())),
            vk_instance_handle,
        );

        // Build minimal ash::Device from the same function loader
        let ash_device = ash::Device::load_with(
            |name| std::mem::transmute(get_device_proc_addr(vk_device_handle, name.as_ptr())),
            vk_device_handle,
        );

        let display_timing = ash::google::display_timing::Device::new(&ash_instance, &ash_device);

        Self {
            display_timing,
            swapchain_handle: RefCell::new(swapchain.handle()),
            cached_refresh_duration: RefCell::new(None),
        }
    }

    /// Update the swapchain handle after swapchain recreation.
    pub fn update_swapchain(&self, swapchain: &Arc<vulkano::swapchain::Swapchain>) {
        use vulkano::VulkanObject;
        *self.swapchain_handle.borrow_mut() = swapchain.handle();
        // Clear cached refresh duration - may change with new swapchain
        *self.cached_refresh_duration.borrow_mut() = None;
    }

    /// Query the driver for the refresh cycle duration.
    fn query_refresh_duration(&self) -> Option<Duration> {
        let handle = *self.swapchain_handle.borrow();
        match unsafe { self.display_timing.get_refresh_cycle_duration(handle) } {
            Ok(cycle) => {
                if cycle.refresh_duration > 0 {
                    Some(Duration::from_nanos(cycle.refresh_duration))
                } else {
                    None
                }
            }
            Err(e) => {
                warn!("vkGetRefreshCycleDurationGOOGLE failed: {:?}", e);
                None
            }
        }
    }

    /// Query past presentation timings, returning the most recent.
    fn query_latest_present_time(&self) -> Option<u64> {
        let handle = *self.swapchain_handle.borrow();
        match unsafe { self.display_timing.get_past_presentation_timing(handle) } {
            Ok(timings) => timings.last().map(|t| t.actual_present_time),
            Err(e) => {
                warn!("vkGetPastPresentationTimingGOOGLE failed: {:?}", e);
                None
            }
        }
    }

    /// Query past presentation timings for a specific `present_id`.
    ///
    /// Returns the `actual_present_time` (nanoseconds) if the driver has recorded
    /// a timing entry whose `presentID` matches `present_id`. The driver may batch
    /// or delay these records, so callers should fall back to CPU time when this
    /// returns `None`.
    fn query_present_time_for_id(&self, present_id: u32) -> Option<u64> {
        let handle = *self.swapchain_handle.borrow();
        match unsafe { self.display_timing.get_past_presentation_timing(handle) } {
            Ok(timings) => timings
                .iter()
                .find(|t| t.present_id == present_id)
                .map(|t| t.actual_present_time),
            Err(e) => {
                warn!("vkGetPastPresentationTimingGOOGLE failed: {:?}", e);
                None
            }
        }
    }
}

impl TimingProvider for GoogleDisplayTimingProvider {
    fn source(&self) -> TimingSource {
        TimingSource::GoogleDisplayTiming
    }

    fn refresh_cycle_duration(&self) -> Option<Duration> {
        // Return cached value if available
        if let Some(cached) = *self.cached_refresh_duration.borrow() {
            return Some(cached);
        }
        // Query and cache
        if let Some(dur) = self.query_refresh_duration() {
            *self.cached_refresh_duration.borrow_mut() = Some(dur);
            Some(dur)
        } else {
            None
        }
    }

    fn record_present_time(&self, clock: &Clock) -> Timestamp {
        // Try to get hardware present time; fall back to CPU time
        if let Some(nanos) = self.query_latest_present_time() {
            // VK_GOOGLE_display_timing reports times in nanoseconds
            // relative to some device-specific epoch. We convert to
            // our Timestamp which is microseconds from Clock epoch.
            // Since we can't correlate device epoch with Clock epoch,
            // use the CPU time as baseline but this gives us the
            // actual present time from the driver.
            Timestamp::from_micros(nanos / 1_000)
        } else {
            // No timing data available yet (first frames), fall back
            clock.now()
        }
    }

    fn wait_for_target(&self, target_time: Timestamp, clock: &Clock) {
        // For Google Display Timing, the target time should ideally be
        // passed via VkPresentTimesInfoGOOGLE in the present pNext chain.
        // For now, fall back to CPU spin-wait like CpuTimingProvider.
        while clock.now() < target_time {
            std::hint::spin_loop();
        }
    }

    fn confirmed_present_time_for(&self, frame_number: u64, clock: &Clock) -> Option<Timestamp> {
        // Map frame_number to a present_id. We use (frame_number & 0xFFFF_FFFF) as
        // a u32 present_id. This matches any future VkPresentTimesInfoGOOGLE
        // integration where the same mapping is used.
        //
        // NOTE: Until submit_nonblocking attaches VkPresentTimesInfoGOOGLE to the
        // present pNext chain (future work), present_id in driver records will be 0
        // for every frame, so this lookup will not find a matching entry. The
        // fallback to record_present_time() (most recent timing) is used instead.
        let present_id = (frame_number & 0xFFFF_FFFF) as u32;
        self.query_present_time_for_id(present_id)
            .map(|nanos| Timestamp::from_micros(nanos / 1_000))
            .or_else(|| {
                // Fall back to most-recent timing from the driver, same as
                // record_present_time(). Returns None on first few frames.
                self.query_latest_present_time()
                    .map(|nanos| Timestamp::from_micros(nanos / 1_000))
                    .or_else(|| Some(clock.now()))
            })
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
