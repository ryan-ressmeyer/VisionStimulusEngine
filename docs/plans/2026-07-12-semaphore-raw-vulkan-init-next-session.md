# Next session: GPU semaphores for the external-frame ring via Bevy `raw_vulkan_init`

## Session goal

Replace the `SyncKind::CpuBlocking` fallback in the Bevy→VSE external-frame handoff with the
already-implemented `SyncKind::BinaryPerSlot` GPU-semaphore path, by getting
`VK_KHR_external_semaphore_fd` enabled on Bevy's wgpu device through Bevy 0.19's
`raw_vulkan_init` feature. Secondary: fix the three pre-existing validation errors found on
2026-07-12.

## Context (read first)

- Branch: `feat/3d-external-frame-seam` (tip `5470cac` + any later commits; NOT merged to main).
  The whole seam works and is verified — see `docs/3d-vr-rendering-landscape.md` §8
  "[2026-07-12] Spike results" and `crates/vse-bevy/examples/01_bevy_ring_demo.rs` header.
- The problem: wgpu 29 never enables `VK_KHR_external_semaphore_fd` at `vkCreateDevice`, so the
  loader returns NULL for `vkGetSemaphoreFdKHR` on Bevy's device. The probe in
  `crates/vse-bevy/src/ring_alloc.rs::probe_semaphore_export` detects this and selects
  `CpuBlocking` (producer stalls per frame; correct but unpipelined).
- Everything downstream of the probe already supports semaphores: exported-semaphore creation
  (`ring_alloc.rs`, gated on `SyncKind::BinaryPerSlot`), `add_signal_semaphore` + empty submit
  (`crates/vse-bevy/src/lib.rs::render_frame`), VSE-side import + submit waits
  (`src/core/external_frame.rs`, `src/core/present_engine.rs::submit_and_present`), and the
  ring state machine's binary reuse invariant (`crates/vse-external-frame/src/ring.rs`).
  **Flipping the probe's answer is the only change needed** — do not restructure the seam.
- `docs/upstream-watch.md` item 2 tracks the long-term fix (wgpu enabling the extension itself).

## Plan

1. Read Bevy 0.19's `raw_vulkan_init` surface before writing code
   (`~/.cargo/registry/src/*/bevy_render-0.19.0/src/renderer/raw_vulkan_init.rs`, feature
   `raw_vulkan_init` in bevy_render's Cargo.toml; upstream PR #20565). Confirm it can add
   **device extensions** at creation (not just instance-level). If it cannot, stop, record the
   finding in `docs/upstream-watch.md`, and keep CpuBlocking.
2. Enable the `raw_vulkan_init` feature on the bevy dependency in `crates/vse-bevy/Cargo.toml`
   and request `VK_KHR_external_semaphore_fd` (+ `VK_KHR_external_memory_fd` if the hook
   replaces rather than extends wgpu's list — check semantics carefully) via the hook in
   `BevyProducer::new`.
3. Rebuild, rerun the probe: `RUST_LOG=info cargo run -p vse-bevy --profile demo --example
   01_bevy_ring_demo 200` should log the BinaryPerSlot probe success line instead of the
   CpuBlocking warning, and the report line `frame sync : BinaryPerSlot`.
4. Verify (all three required before claiming done):
   - Determinism: `02_verify_determinism` twice — hashes byte-identical across runs, distinct
     across frames (the semaphore path must not change pixels).
   - Timing: 1000-frame release run (`--release`, not `--profile demo`) — stats comparable or
     better than the CpuBlocking baseline recorded in the 01 example header (3–15 missed/999).
   - Validation: with `VK_LOADER_LAYERS_ENABLE='*validation*'`, no NEW validation errors vs the
     2026-07-12 baseline (see below), especially no semaphore lifecycle errors
     (double-signal / wait-without-signal would indicate a ring-invariant violation).
5. Secondary fix (pre-existing, main's B3 code): `vkCreateDevice` validation errors
   `VUID-vkCreateDevice-ppEnabledExtensionNames-01387` — `src/core/present_timing_ext.rs`
   `ext_names` must also list the dependencies of the present-timing family:
   `VK_KHR_get_surface_capabilities2` (required by `VK_EXT_present_timing`,
   `VK_KHR_present_id2`, `VK_KHR_present_wait2`) and `VK_EXT_calibrated_timestamps` must be
   enabled *unconditionally* when present-timing is (it is currently conditional on
   `advertise_calibrated`). Gate on `has_ext`, mirror into the vulkano `DeviceCreateInfo`
   where a field exists. Re-run validation to confirm those three errors disappear.
6. Update `docs/upstream-watch.md` item 2 (workaround now = raw_vulkan_init shim), the §8
   results block (item 2 finding), and the 01 example header stats if they changed.

## Validation baseline (2026-07-12, for "no NEW errors" comparison)

- `VUID-StandaloneSpirv-None-10684` ×10 (shader layout pedantry, source not yet triaged)
- `VUID-vkCreateDevice-ppEnabledExtensionNames-01387` ×3 (fixed by step 5)
- `VUID-vkSetSwapchainPresentTimingQueueSizeEXT-swapchain-12229` ×2 (pre-existing B3 quirk)
- `VUID-vkAcquireNextImageKHR-swapchain-parameter` ×1 (swapchain recreation edge)

## Gotchas

- Build with `CARGO_INCREMENTAL=0`; iterate with `--profile demo` (no fat LTO) + mold; final
  timing numbers on `--release`. See memory `vse-build-and-hardware-gotchas`.
- The binary-semaphore reuse invariant: a slot's semaphore may be re-signaled only after the
  consumer's fence confirms the wait executed (`release()` path). The drain-on-flip logic in
  `src/core/external_frame.rs::take_frames` exists to keep this true across skipped flips
  (Wayland startup OUT_OF_DATE) — don't bypass it.
- `RenderCreation::default()` is set in `BevyProducer::new`; `raw_vulkan_init` may require
  configuring `WgpuSettings` / a different `RenderCreation` — keep the stock wgpu device
  (Topology 2); do NOT switch to `RenderCreation::Manual`.
