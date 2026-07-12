# Upstream watch list

Features VSE is waiting on from upstream projects, with the workaround currently in place and
how to check for movement. Re-check on each upstream release (or ~quarterly). When an item
lands, the "Unblocks" column says what to do in VSE.

*Started 2026-07-12 (3D external-frame seam session).*

| # | Waiting for | Current workaround | How to check | Unblocks |
|---|---|---|---|---|
| 1 | **Bevy upgrading to wgpu 30+** (Bevy 0.20?). wgpu 30 adds `texture_from_dmabuf_fd` and `Queue::add_wait_semaphore`. | Ring images allocated with raw ash + OPAQUE_FD (`crates/vse-bevy/src/ring_alloc.rs`); no wait-semaphore into the producer (release back-edge is a CPU channel). | Bevy release notes / `cargo tree -p bevy_render -i wgpu` after a Bevy bump. | Simpler ring allocation (drop most of `ring_alloc.rs` raw-ash code); dmabuf path for cross-process/cross-vendor; GPU wait edge into the producer. |
| 2 | **wgpu enabling `VK_KHR_external_semaphore_fd`** on its Vulkan device (or an API to request extra device extensions). | Bevy 0.19 `raw_vulkan_init` feature to request the extension at device creation (in progress); before that, `SyncKind::CpuBlocking` fallback. | wgpu CHANGELOG (search "external_semaphore"); `wgpu-hal/src/vulkan/adapter.rs` extension list. | Delete the `raw_vulkan_init` shim; semaphore export becomes spec-clean on stock wgpu. |
| 3 | **ANV implementing the advertised present-timing sub-features**: `vkGetPastPresentationTimingEXT` scanout stage timestamps (currently stubbed to 0) and `targetTime` enforcement (currently ignored). Mesa 26.1 measured. | Behavioral probes + `present_time` sampled from the calibrated scanout clock; software pacing (`CLAUDE.md` driver-conformance caveat, `docs/clock-synchronization.md` §6). | Re-run `examples/11_raw_present_feedback` + `examples/13_direct_display_scanout` after each Mesa upgrade; check `scanout_feedback_populated` / `absolute_scheduling_enforced` in HostInfo. | Real per-present hardware scanout timestamps; hardware-enforced scheduled flips. |
| 4 | **vulkano gaining Vulkan 1.4 present-timing types** (vulkano > 0.35). | Hand-declared structs + raw `vkCreateDevice` in `src/core/present_timing_ext.rs` (ABI-guarded by size asserts). | vulkano releases / its `VulkanoVersion` supported-spec bump. | Drop the hand-declared structs and possibly the raw device-creation path. |
| 5 | **ash releasing present-timing (Vulkan 1.4) definitions** (ash > 0.38). | Same hand-declared structs as #4. | ash CHANGELOG. | Replace hand-declared structs with ash's; keep the raw create if vulkano still lags. |
| 6 | **ANV offering HIGH global priority without `CAP_SYS_NICE`** (kernel/Mesa scheduler policy — may never change). | `QueuePriorityOutcome` recorded in `HostInfo.timing`; `setcap 'cap_sys_nice=eip' <binary>` for privileged runs; AMD/RADV rig for the real QoS measurement. | Re-run the queue-family priority probe (scratch tool or `RUST_LOG=info` any example: `queue_priority=` in the backend log line). | HIGH-priority present queue on the dev laptop without setcap. |
