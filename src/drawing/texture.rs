/// Handle to a loaded texture.
///
/// This is a lightweight identifier. The actual GPU resources are
/// managed internally by the Renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureHandle {
    pub(crate) id: u64,
    /// Width of the texture in pixels
    pub width: u32,
    /// Height of the texture in pixels
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_texture_handle_copy() {
        let h = TextureHandle {
            id: 1,
            width: 256,
            height: 256,
        };
        let h2 = h; // Copy
        assert_eq!(h, h2);
    }
}
