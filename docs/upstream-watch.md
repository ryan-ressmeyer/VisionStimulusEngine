# Upstream watch list

Features VSE is waiting on from upstream projects, with the workaround currently in place and
how to check for movement. Re-check on each upstream release (or ~quarterly). When an item
lands, the "Unblocks" column says what to do in VSE.

*Started 2026-07-12 (3D external-frame seam session).*

| # | Waiting for | Current workaround | How to check | Unblocks |
|---|---|---|---|---|
| 1 | **Bevy upgrading to wgpu 30+** (Bevy 0.20?). wgpu 30 adds `texture_from_dmabuf_fd` and `Queue::add_wait_semaphore`. | Ring images allocated with raw ash + OPAQUE_FD (`crates/vse-bevy/src/ring_alloc.rs`); no wait-semaphore into the producer (release back-edge is a CPU channel). | Bevy release notes / `cargo tree -p bevy_render -i wgpu` after a Bevy bump. | Simpler ring allocation (drop most of `ring_alloc.rs` raw-ash code); dmabuf path for cross-process/cross-vendor; GPU wait edge into the producer. |
| 2 | **wgpu enabling `VK_KHR_external_semaphore_fd`** on its Vulkan device (or an API to request extra device extensions). | **Shim in place (2026-07-12):** Bevy 0.19 `raw_vulkan_init` device callback appends the extension to wgpu's list at `vkCreateDevice` (`BevyProducer::new`, gated on `supports_extension`); probe now selects `SyncKind::BinaryPerSlot`, with `CpuBlocking` fallback retained if the hook ever stops running. | wgpu CHANGELOG (search "external_semaphore"); `wgpu-hal/src/vulkan/adapter.rs` extension list. | Delete the `raw_vulkan_init` shim; semaphore export becomes spec-clean on stock wgpu. |
| 3 | **Present-timing behavior changing across Mesa, kernel, compositor, or display-path updates.** The corrected 2026-08-03 reference run found populated feedback and direct-display target enforcement after VSE enabled the required swapchain timing bit. | Per-frame timestamp provenance, synchronous software pacing, and recorded advertised/observed capabilities; see `docs/timing-conformance.md`. | Re-run examples 10–13 after relevant upgrades and compare validation-clean results on the actual recording path. | Update the dated reference measurement and any path-specific workaround; do not infer behavior from extension strings. |
| 4 | **vulkano gaining Vulkan 1.4 present-timing types** (vulkano > 0.35). | Hand-declared structs + raw `vkCreateDevice` in `src/core/present_timing_ext.rs` (ABI-guarded by size asserts). | vulkano releases / its `VulkanoVersion` supported-spec bump. | Drop the hand-declared structs and possibly the raw device-creation path. |
| 5 | **ash releasing present-timing (Vulkan 1.4) definitions** (ash > 0.38). | Same hand-declared structs as #4. | ash CHANGELOG. | Replace hand-declared structs with ash's; keep the raw create if vulkano still lags. |
| 6 | **ANV offering HIGH global priority without `CAP_SYS_NICE`** (kernel/Mesa scheduler policy — may never change). | `QueuePriorityOutcome` recorded in `HostInfo.timing`; `setcap 'cap_sys_nice=eip' <binary>` for privileged runs; AMD/RADV rig for the real QoS measurement. | Re-run the queue-family priority probe (scratch tool or `RUST_LOG=info` any example: `queue_priority=` in the backend log line). | HIGH-priority present queue on the dev laptop without setcap. |
