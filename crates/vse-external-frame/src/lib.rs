//! Wire types and shared logic for VSE's external-renderer handoff seam.
//!
//! This crate is deliberately dependency-free (no vulkano, no wgpu): both the
//! producer side (e.g. `vse-bevy`) and the consumer side (the VSE core crate)
//! name these types without pulling in each other's graphics stack. Everything
//! that crosses the device boundary is a plain file descriptor or integer.

mod format;
mod ring;

pub use format::{negotiate_format, NegotiateError, RingFormat};
pub use ring::{
    release_channel, RingError, RingStateMachine, SlotIndex, SlotReleaseRx, SlotReleaseTx,
    SlotState, SyncKind,
};

/// One exported image of the ring, described by what the importer needs.
///
/// OPAQUE_FD for the proof of concept (same driver on both devices). The
/// dmabuf + explicit DRM-format-modifier upgrade adds fields here (modifier,
/// per-plane layouts) without changing the consumer API.
pub struct ExternalImageDesc {
    /// One opaque FD per image; each image uses a dedicated allocation.
    pub memory_fd: std::os::fd::OwnedFd,
    /// Exact allocation size on the exporting device.
    pub allocation_size: u64,
    /// Memory type index on the exporter. Valid on the importer only because
    /// both devices sit on the same physical device (OPAQUE_FD contract).
    pub memory_type_index: u32,
    pub format: RingFormat,
    pub extent: [u32; 2],
}

/// The full ring as handed from producer to consumer.
///
/// Layout contract (documented, not negotiated, for the PoC): the producer
/// leaves each finished frame in `COLOR_ATTACHMENT_OPTIMAL`; the consumer must
/// return the image to that layout by the end of the command buffer that
/// samples it.
pub struct ExternalRingDesc {
    pub images: Vec<ExternalImageDesc>,
    /// Per-slot binary ready-semaphore FDs; empty when `sync` is `CpuBlocking`.
    pub ready_semaphore_fds: Vec<std::os::fd::OwnedFd>,
    pub sync: SyncKind,
}
