use std::ops::{Deref, DerefMut};

/// Cache-line aligned wrapper preventing false sharing.
/// Uses 128-byte alignment (Apple Silicon L1 cache line size).
#[repr(align(128))]
pub struct CacheAligned<T>(pub T);

impl<T> Deref for CacheAligned<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CacheAligned<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

// SAFETY: CacheAligned is a transparent wrapper; Send/Sync depend on T.
unsafe impl<T: Send> Send for CacheAligned<T> {}
unsafe impl<T: Sync> Sync for CacheAligned<T> {}
