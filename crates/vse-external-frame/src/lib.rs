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
    /// Per-slot binary ready-semaphore FDs; empty when `sync` is `CpuBlocking`
    /// or `Timeline`.
    pub ready_semaphore_fds: Vec<std::os::fd::OwnedFd>,
    /// One timeline semaphore FD; present only when `sync` is `Timeline`.
    pub timeline_semaphore_fd: Option<std::os::fd::OwnedFd>,
    pub sync: SyncKind,
}

impl ExternalRingDesc {
    pub fn validate_sync_shape(&self, ring_len: usize) -> Result<(), String> {
        match self.sync {
            SyncKind::BinaryPerSlot => {
                if self.ready_semaphore_fds.len() != ring_len {
                    return Err(format!(
                        "{} binary semaphore fd(s) for {ring_len} image(s)",
                        self.ready_semaphore_fds.len()
                    ));
                }
                if self.timeline_semaphore_fd.is_some() {
                    return Err("BinaryPerSlot ring must not carry a timeline semaphore fd".into());
                }
            }
            SyncKind::Timeline => {
                if !self.ready_semaphore_fds.is_empty() {
                    return Err("Timeline ring must not carry per-slot binary semaphore fds".into());
                }
                if self.timeline_semaphore_fd.is_none() {
                    return Err("Timeline ring must carry one timeline semaphore fd".into());
                }
            }
            SyncKind::CpuBlocking => {
                if !self.ready_semaphore_fds.is_empty() {
                    return Err("CpuBlocking ring must not carry binary semaphore fds".into());
                }
                if self.timeline_semaphore_fd.is_some() {
                    return Err("CpuBlocking ring must not carry a timeline semaphore fd".into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    fn fake_fd() -> OwnedFd {
        std::fs::File::open("/dev/null")
            .expect("open /dev/null")
            .into()
    }

    #[test]
    fn timeline_descriptors_carry_one_timeline_fd_and_no_binary_fds() {
        let desc = ExternalRingDesc {
            images: Vec::new(),
            ready_semaphore_fds: Vec::new(),
            timeline_semaphore_fd: Some(fake_fd()),
            sync: SyncKind::Timeline,
        };

        assert_eq!(desc.sync, SyncKind::Timeline);
        assert!(desc.ready_semaphore_fds.is_empty());
        assert!(desc.timeline_semaphore_fd.is_some());
    }

    #[test]
    fn sync_shape_validation_rejects_mixed_timeline_and_binary_fds() {
        let desc = ExternalRingDesc {
            images: Vec::new(),
            ready_semaphore_fds: vec![fake_fd()],
            timeline_semaphore_fd: Some(fake_fd()),
            sync: SyncKind::Timeline,
        };

        assert!(desc.validate_sync_shape(3).is_err());
    }
}
