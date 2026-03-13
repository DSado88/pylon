use std::sync::atomic::{AtomicPtr, Ordering};
use std::ptr;

/// A lock-free write-once slot. The first writer wins; subsequent writes are dropped.
pub struct WriteOnceSlot<T> {
    ptr: AtomicPtr<T>,
}

impl<T> WriteOnceSlot<T> {
    pub const fn new() -> Self {
        Self {
            ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Try to store a value. Returns a reference to the stored value.
    /// If another thread won the race, the provided value is dropped and
    /// a reference to the winner's value is returned.
    pub fn try_store(&self, value: T) -> &T {
        let boxed = Box::into_raw(Box::new(value));
        match self.ptr.compare_exchange(
            ptr::null_mut(),
            boxed,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // We won the race
                // SAFETY: we just stored this pointer and it won't be freed until Drop
                unsafe { &*boxed }
            }
            Err(winner) => {
                // We lost the race; free our allocation
                // SAFETY: boxed was just allocated by us and nobody else has it
                drop(unsafe { Box::from_raw(boxed) });
                // SAFETY: winner was stored by the winning thread and lives until Drop
                unsafe { &*winner }
            }
        }
    }

    /// Get a reference to the stored value, if any.
    pub fn get(&self) -> Option<&T> {
        let p = self.ptr.load(Ordering::Acquire);
        if p.is_null() {
            None
        } else {
            // SAFETY: non-null means it was stored via try_store and lives until Drop
            Some(unsafe { &*p })
        }
    }
}

impl<T> Drop for WriteOnceSlot<T> {
    fn drop(&mut self) {
        let p = *self.ptr.get_mut();
        if !p.is_null() {
            // SAFETY: we own this allocation and no more references can be created
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

impl<T> Default for WriteOnceSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: WriteOnceSlot manages a heap allocation via AtomicPtr.
// Send/Sync are safe if T is Send (value can be sent across threads)
// and Sync (shared references are safe across threads).
unsafe impl<T: Send + Sync> Send for WriteOnceSlot<T> {}
unsafe impl<T: Send + Sync> Sync for WriteOnceSlot<T> {}
