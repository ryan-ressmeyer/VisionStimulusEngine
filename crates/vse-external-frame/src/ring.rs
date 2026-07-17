//! The ring-slot state machine shared by producer and consumer.
//!
//! Each side runs its own `RingStateMachine` mirror over the same slot
//! indices. States: `Free → Producing → Ready → Consuming → Free`.
//!
//! The machine encodes the binary-semaphore reuse rule as a hard invariant:
//! a binary semaphore must not be re-signaled until its previous signal has
//! been waited. A slot's semaphore is signaled when the producer finishes it
//! (`Producing → Ready`) and waited by the consumer's submit (`Ready →
//! Consuming`). The slot only returns to `Free` — i.e. becomes re-signalable —
//! via [`RingStateMachine::release`], which the consumer calls after the fence
//! of the submit that *waited* the semaphore has signaled. Under
//! [`SyncKind::BinaryPerSlot`], [`RingStateMachine::take_ready`] is FIFO so
//! every signaled slot is eventually waited before reuse.

use std::collections::VecDeque;
use std::sync::mpsc;

/// Index of a slot in the external image ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotIndex(pub usize);

/// Lifecycle state of one ring slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Free,
    Producing,
    Ready,
    Consuming,
}

/// How frame-completion crosses the device boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    /// One exported binary semaphore per slot (PoC default).
    BinaryPerSlot,
    /// One exported timeline semaphore, value = produce counter (future).
    Timeline,
    /// Loud fallback: producer blocks on the CPU until its GPU work is done;
    /// no semaphores cross the boundary.
    CpuBlocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RingError {
    #[error("ring of {len} slot(s) is too small; need at least 2")]
    TooSmall { len: usize },
    #[error("no free slot to produce into (ring too small for the pipeline depth?)")]
    NoFreeSlot,
    #[error("slot {slot} is {actual:?}, expected {expected:?}")]
    InvalidTransition {
        slot: usize,
        actual: SlotState,
        expected: SlotState,
    },
    #[error("slot {slot} out of range for ring of {len}")]
    OutOfRange { slot: usize, len: usize },
}

/// Per-side authoritative slot state machine.
#[derive(Debug)]
pub struct RingStateMachine {
    slots: Vec<SlotState>,
    /// Slots in `Ready` state, oldest first (order of `mark_ready`).
    ready_order: VecDeque<usize>,
    sync: SyncKind,
}

impl RingStateMachine {
    /// A ring needs at least 2 slots (one producing, one consuming).
    pub fn new(len: usize, sync: SyncKind) -> Result<Self, RingError> {
        if len < 2 {
            return Err(RingError::TooSmall { len });
        }
        Ok(Self {
            slots: vec![SlotState::Free; len],
            ready_order: VecDeque::new(),
            sync,
        })
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn sync(&self) -> SyncKind {
        self.sync
    }

    /// Claim a `Free` slot for rendering (`Free → Producing`).
    pub fn acquire_for_produce(&mut self) -> Result<SlotIndex, RingError> {
        let slot = self
            .slots
            .iter()
            .position(|s| *s == SlotState::Free)
            .ok_or(RingError::NoFreeSlot)?;
        self.slots[slot] = SlotState::Producing;
        Ok(SlotIndex(slot))
    }

    /// Claim a specific producer-chosen `Free` slot (`Free → Producing`).
    ///
    /// Consumer mirrors use this when an asynchronous producer reports which
    /// slot it actually rendered into. Release messages can reach the producer
    /// and consumer mirrors at different times, so lowest-free acquisition is
    /// not a safe cross-thread assumption.
    pub fn acquire_specific_for_produce(&mut self, slot: SlotIndex) -> Result<(), RingError> {
        self.expect_state(slot, SlotState::Free)?;
        self.slots[slot.0] = SlotState::Producing;
        Ok(())
    }

    /// The producer finished the slot's frame and signaled its semaphore
    /// (`Producing → Ready`).
    pub fn mark_ready(&mut self, slot: SlotIndex) -> Result<(), RingError> {
        self.expect_state(slot, SlotState::Producing)?;
        self.slots[slot.0] = SlotState::Ready;
        self.ready_order.push_back(slot.0);
        Ok(())
    }

    /// Hand a ready slot to the consumer (`Ready → Consuming`).
    ///
    /// Under `BinaryPerSlot` (and `CpuBlocking`) this is FIFO — every signaled
    /// semaphore must eventually be waited. Under `Timeline` it takes the
    /// newest ready slot and returns older ready slots straight to `Free`
    /// (their signal values are superseded, never waited).
    pub fn take_ready(&mut self) -> Option<SlotIndex> {
        match self.sync {
            SyncKind::BinaryPerSlot | SyncKind::CpuBlocking => {
                let slot = self.ready_order.pop_front()?;
                self.slots[slot] = SlotState::Consuming;
                Some(SlotIndex(slot))
            }
            SyncKind::Timeline => {
                let slot = self.ready_order.pop_back()?;
                // Older ready frames are superseded; their values are never
                // waited, so the slots go straight back to Free.
                for stale in self.ready_order.drain(..) {
                    self.slots[stale] = SlotState::Free;
                }
                self.slots[slot] = SlotState::Consuming;
                Some(SlotIndex(slot))
            }
        }
    }

    /// Hand every ready slot to the consumer in production order
    /// (`Ready → Consuming`). Unlike [`take_ready`](Self::take_ready), timeline
    /// stale slots are not freed immediately; the caller can wait the newest
    /// timeline value and then release every superseded slot through the normal
    /// back-edge.
    pub fn take_all_ready(&mut self) -> Vec<SlotIndex> {
        let mut slots = Vec::with_capacity(self.ready_order.len());
        while let Some(slot) = self.ready_order.pop_front() {
            self.slots[slot] = SlotState::Consuming;
            slots.push(SlotIndex(slot));
        }
        slots
    }

    /// The consumer's submit that sampled the slot has fully executed
    /// (`Consuming → Free`); the slot's semaphore may be re-signaled.
    pub fn release(&mut self, slot: SlotIndex) -> Result<(), RingError> {
        self.expect_state(slot, SlotState::Consuming)?;
        self.slots[slot.0] = SlotState::Free;
        Ok(())
    }

    /// Minimum ring size for a consumer pipeline `depth` frames deep:
    /// one producing + `depth + 1` potentially un-released consumed slots.
    pub fn min_ring_for_depth(depth: usize) -> usize {
        depth + 2
    }

    fn expect_state(&self, slot: SlotIndex, expected: SlotState) -> Result<(), RingError> {
        let actual = *self.slots.get(slot.0).ok_or(RingError::OutOfRange {
            slot: slot.0,
            len: self.slots.len(),
        })?;
        if actual != expected {
            return Err(RingError::InvalidTransition {
                slot: slot.0,
                actual,
                expected,
            });
        }
        Ok(())
    }
}

/// Sender half of the consumer→producer slot-release back-edge.
#[derive(Clone)]
pub struct SlotReleaseTx(mpsc::Sender<SlotIndex>);

/// Receiver half of the consumer→producer slot-release back-edge.
pub struct SlotReleaseRx(mpsc::Receiver<SlotIndex>);

impl SlotReleaseTx {
    /// Best-effort: a dropped receiver (producer shut down) is not an error
    /// the consumer can act on.
    pub fn send(&self, slot: SlotIndex) {
        let _ = self.0.send(slot);
    }
}

impl SlotReleaseRx {
    /// Drain all pending releases without blocking.
    pub fn drain(&self) -> impl Iterator<Item = SlotIndex> + '_ {
        std::iter::from_fn(move || self.0.try_recv().ok())
    }
}

/// The consumer sends released `SlotIndex`es; the producer drains them before
/// acquiring. Off the critical path by design.
pub fn release_channel() -> (SlotReleaseTx, SlotReleaseRx) {
    let (tx, rx) = mpsc::channel();
    (SlotReleaseTx(tx), SlotReleaseRx(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(len: usize, sync: SyncKind) -> RingStateMachine {
        RingStateMachine::new(len, sync).unwrap()
    }

    #[test]
    fn new_rejects_len_below_2() {
        assert_eq!(
            RingStateMachine::new(0, SyncKind::BinaryPerSlot).unwrap_err(),
            RingError::TooSmall { len: 0 }
        );
        assert_eq!(
            RingStateMachine::new(1, SyncKind::BinaryPerSlot).unwrap_err(),
            RingError::TooSmall { len: 1 }
        );
        assert_eq!(ring(2, SyncKind::BinaryPerSlot).len(), 2);
    }

    #[test]
    fn acquire_only_from_free_and_errors_when_exhausted() {
        let mut m = ring(3, SyncKind::BinaryPerSlot);
        let a = m.acquire_for_produce().unwrap();
        let b = m.acquire_for_produce().unwrap();
        let c = m.acquire_for_produce().unwrap();
        let mut ids = [a.0, b.0, c.0];
        ids.sort();
        assert_eq!(ids, [0, 1, 2], "three acquires must yield distinct slots");
        assert_eq!(m.acquire_for_produce().unwrap_err(), RingError::NoFreeSlot);
    }

    #[test]
    fn acquire_specific_claims_producer_chosen_free_slot() {
        let mut m = ring(4, SyncKind::BinaryPerSlot);
        let a = m.acquire_for_produce().unwrap();
        let b = m.acquire_for_produce().unwrap();
        m.mark_ready(a).unwrap();
        m.mark_ready(b).unwrap();
        assert_eq!(m.take_all_ready(), vec![a, b]);
        m.release(b).unwrap();
        m.release(a).unwrap();

        // Both slots are free again, but a consumer mirror may receive a
        // producer handoff for the later slot first. It must claim that exact
        // slot rather than assuming lowest-free FIFO acquisition.
        m.acquire_specific_for_produce(b).unwrap();
        assert_eq!(
            m.mark_ready(a).unwrap_err(),
            RingError::InvalidTransition {
                slot: a.0,
                actual: SlotState::Free,
                expected: SlotState::Producing,
            }
        );
        m.mark_ready(b).unwrap();
    }

    #[test]
    fn mark_ready_requires_producing() {
        let mut m = ring(3, SyncKind::BinaryPerSlot);
        // Slot 0 is Free, not Producing.
        assert_eq!(
            m.mark_ready(SlotIndex(0)).unwrap_err(),
            RingError::InvalidTransition {
                slot: 0,
                actual: SlotState::Free,
                expected: SlotState::Producing,
            }
        );
        let s = m.acquire_for_produce().unwrap();
        m.mark_ready(s).unwrap();
        // Double mark_ready: now Ready, not Producing.
        assert_eq!(
            m.mark_ready(s).unwrap_err(),
            RingError::InvalidTransition {
                slot: s.0,
                actual: SlotState::Ready,
                expected: SlotState::Producing,
            }
        );
    }

    #[test]
    fn out_of_range_slot_is_rejected() {
        let mut m = ring(2, SyncKind::BinaryPerSlot);
        assert_eq!(
            m.mark_ready(SlotIndex(7)).unwrap_err(),
            RingError::OutOfRange { slot: 7, len: 2 }
        );
        assert_eq!(
            m.release(SlotIndex(7)).unwrap_err(),
            RingError::OutOfRange { slot: 7, len: 2 }
        );
    }

    #[test]
    fn binary_take_ready_is_fifo() {
        let mut m = ring(3, SyncKind::BinaryPerSlot);
        let a = m.acquire_for_produce().unwrap();
        m.mark_ready(a).unwrap();
        let b = m.acquire_for_produce().unwrap();
        m.mark_ready(b).unwrap();
        // Oldest signal first: every binary signal must eventually be waited.
        assert_eq!(m.take_ready(), Some(a));
        assert_eq!(m.take_ready(), Some(b));
        assert_eq!(m.take_ready(), None);
    }

    #[test]
    fn timeline_take_ready_is_latest_and_frees_stale() {
        let mut m = ring(3, SyncKind::Timeline);
        let a = m.acquire_for_produce().unwrap();
        m.mark_ready(a).unwrap();
        let b = m.acquire_for_produce().unwrap();
        m.mark_ready(b).unwrap();
        // Newest frame wins; the stale ready slot goes straight back to Free.
        assert_eq!(m.take_ready(), Some(b));
        assert_eq!(m.take_ready(), None);
        // Stale slot `a` must be acquirable again without a release().
        let c = m.acquire_for_produce().unwrap();
        let d = m.acquire_for_produce().unwrap();
        assert!([c.0, d.0].contains(&a.0), "stale ready slot was not freed");
    }

    #[test]
    fn take_all_ready_keeps_timeline_stale_slots_consuming_until_release() {
        let mut m = ring(3, SyncKind::Timeline);
        let a = m.acquire_for_produce().unwrap();
        m.mark_ready(a).unwrap();
        let b = m.acquire_for_produce().unwrap();
        m.mark_ready(b).unwrap();

        assert_eq!(m.take_all_ready(), vec![a, b]);
        assert_eq!(m.acquire_for_produce().unwrap().0, 2);
        assert_eq!(m.acquire_for_produce().unwrap_err(), RingError::NoFreeSlot);

        m.release(a).unwrap();
        assert_eq!(m.acquire_for_produce().unwrap(), a);
    }

    #[test]
    fn release_only_from_consuming() {
        let mut m = ring(3, SyncKind::BinaryPerSlot);
        let a = m.acquire_for_produce().unwrap();
        // Producing → Free is illegal.
        assert_eq!(
            m.release(a).unwrap_err(),
            RingError::InvalidTransition {
                slot: a.0,
                actual: SlotState::Producing,
                expected: SlotState::Consuming,
            }
        );
        m.mark_ready(a).unwrap();
        // Ready → Free is illegal too (the semaphore was signaled and must be
        // waited before the slot can be reused).
        assert_eq!(
            m.release(a).unwrap_err(),
            RingError::InvalidTransition {
                slot: a.0,
                actual: SlotState::Ready,
                expected: SlotState::Consuming,
            }
        );
        let taken = m.take_ready().unwrap();
        assert_eq!(taken, a);
        m.release(a).unwrap();
        // Released slot is acquirable again.
        assert!(m.acquire_for_produce().is_ok());
    }

    #[test]
    fn wraparound_3_slot_depth_1_never_starves() {
        // Simulate the PoC pipeline: consumer releases a slot one frame after
        // consuming it (the fence of frame n signals during frame n+1). With a
        // 3-slot ring this must run forever without NoFreeSlot.
        let mut m = ring(3, SyncKind::BinaryPerSlot);
        let mut pending: VecDeque<SlotIndex> = VecDeque::new();
        for frame in 0..100 {
            let p = m
                .acquire_for_produce()
                .unwrap_or_else(|e| panic!("starved at frame {frame}: {e}"));
            m.mark_ready(p).unwrap();
            let c = m.take_ready().expect("just-marked slot must be takeable");
            pending.push_back(c);
            if pending.len() > 1 {
                m.release(pending.pop_front().unwrap()).unwrap();
            }
        }
    }

    #[test]
    fn min_ring_for_depth_is_depth_plus_2() {
        assert_eq!(RingStateMachine::min_ring_for_depth(0), 2);
        assert_eq!(RingStateMachine::min_ring_for_depth(1), 3);
        assert_eq!(RingStateMachine::min_ring_for_depth(2), 4);
    }

    #[test]
    fn release_channel_drains_in_order_and_survives_dropped_rx() {
        let (tx, rx) = release_channel();
        tx.send(SlotIndex(2));
        tx.send(SlotIndex(0));
        let drained: Vec<_> = rx.drain().collect();
        assert_eq!(drained, vec![SlotIndex(2), SlotIndex(0)]);
        assert_eq!(rx.drain().count(), 0);
        drop(rx);
        tx.send(SlotIndex(1)); // must not panic
    }
}
