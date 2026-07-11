//! Ring image formats and producer/consumer format negotiation.
//!
//! Formats both sides can name without depending on vulkano or wgpu. Each
//! variant corresponds to exactly one `VkFormat` (and one wgpu format name).

/// Pixel formats supported for the external image ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingFormat {
    /// `VK_FORMAT_R8G8B8A8_SRGB` / wgpu `Rgba8UnormSrgb`
    Rgba8UnormSrgb,
    /// `VK_FORMAT_B8G8R8A8_SRGB` / wgpu `Bgra8UnormSrgb`
    Bgra8UnormSrgb,
    /// `VK_FORMAT_R16G16B16A16_SFLOAT` / wgpu `Rgba16Float`
    Rgba16Float,
}

impl RingFormat {
    /// The raw `VkFormat` value (spec-pinned; see the unit tests).
    pub fn vk_format(&self) -> u32 {
        match self {
            RingFormat::Rgba8UnormSrgb => 43,
            RingFormat::Bgra8UnormSrgb => 50,
            RingFormat::Rgba16Float => 97,
        }
    }
}

/// No format offered by the producer is accepted by the consumer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no common ring format: producer offers {producer_offers:?}, consumer accepts {consumer_accepts:?}")]
pub struct NegotiateError {
    pub producer_offers: Vec<RingFormat>,
    pub consumer_accepts: Vec<RingFormat>,
}

/// Pick the ring format: the first producer-offered format the consumer
/// accepts wins (producer preference order is authoritative).
pub fn negotiate_format(
    producer_offers: &[RingFormat],
    consumer_accepts: &[RingFormat],
) -> Result<RingFormat, NegotiateError> {
    producer_offers
        .iter()
        .find(|f| consumer_accepts.contains(f))
        .copied()
        .ok_or_else(|| NegotiateError {
            producer_offers: producer_offers.to_vec(),
            consumer_accepts: consumer_accepts.to_vec(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vk_format_values_match_spec() {
        // Pinned to the Vulkan spec's VkFormat values; a typo here corrupts
        // every cross-device image. Mirrors the repo's spec-pinning tests.
        assert_eq!(RingFormat::Rgba8UnormSrgb.vk_format(), 43);
        assert_eq!(RingFormat::Bgra8UnormSrgb.vk_format(), 50);
        assert_eq!(RingFormat::Rgba16Float.vk_format(), 97);
    }

    #[test]
    fn negotiate_picks_first_producer_offer_consumer_accepts() {
        let offers = [RingFormat::Rgba16Float, RingFormat::Rgba8UnormSrgb];
        let accepts = [RingFormat::Bgra8UnormSrgb, RingFormat::Rgba8UnormSrgb];
        assert_eq!(
            negotiate_format(&offers, &accepts),
            Ok(RingFormat::Rgba8UnormSrgb)
        );
    }

    #[test]
    fn negotiate_respects_producer_preference_order() {
        let offers = [RingFormat::Rgba8UnormSrgb, RingFormat::Bgra8UnormSrgb];
        let accepts = [RingFormat::Bgra8UnormSrgb, RingFormat::Rgba8UnormSrgb];
        // Consumer lists Bgra first, but the producer's first acceptable offer wins.
        assert_eq!(
            negotiate_format(&offers, &accepts),
            Ok(RingFormat::Rgba8UnormSrgb)
        );
    }

    #[test]
    fn negotiate_error_when_disjoint() {
        let offers = [RingFormat::Rgba16Float];
        let accepts = [RingFormat::Rgba8UnormSrgb];
        let err = negotiate_format(&offers, &accepts).unwrap_err();
        assert_eq!(err.producer_offers, vec![RingFormat::Rgba16Float]);
        assert_eq!(err.consumer_accepts, vec![RingFormat::Rgba8UnormSrgb]);
    }
}
