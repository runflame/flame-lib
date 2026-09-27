//! A counting allocator, for the peak heap of one send.
//!
//! The `send` bench and `first-send` install one as their global allocator:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: Counting = Counting::new();
//! ```
//!
//! It counts only between [`Counting::start`] and [`Counting::stop`], and
//! then only the net change: bytes allocated minus bytes freed since
//! `start`. What it reports is the highest that change reached, so the
//! heap already in use when counting starts, such as a generator table
//! built earlier, is not part of it. Outside a measurement it costs one
//! relaxed load per call, so criterion's timings in the same binary are
//! unaffected.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

/// The system allocator, counting while a measurement runs.
pub struct Counting {
    counting: AtomicBool,
    /// Bytes allocated minus bytes freed since `start`. Negative when more
    /// was freed than allocated.
    current: AtomicIsize,
    /// The highest `current` reached since `start`.
    peak: AtomicIsize,
}

impl Counting {
    pub const fn new() -> Counting {
        Counting {
            counting: AtomicBool::new(false),
            current: AtomicIsize::new(0),
            peak: AtomicIsize::new(0),
        }
    }

    /// Starts a measurement from zero.
    pub fn start(&self) {
        self.current.store(0, Ordering::SeqCst);
        self.peak.store(0, Ordering::SeqCst);
        self.counting.store(true, Ordering::SeqCst);
    }

    /// Stops the measurement and returns its peak: the most bytes in use at
    /// once beyond what was in use at `start`.
    pub fn stop(&self) -> u64 {
        self.counting.store(false, Ordering::SeqCst);
        self.peak.load(Ordering::SeqCst).max(0) as u64
    }

    fn grow(&self, bytes: usize) {
        let now = self.current.fetch_add(bytes as isize, Ordering::Relaxed) + bytes as isize;
        self.peak.fetch_max(now, Ordering::Relaxed);
    }

    fn shrink(&self, bytes: usize) {
        self.current.fetch_sub(bytes as isize, Ordering::Relaxed);
    }

    fn on(&self) -> bool {
        self.counting.load(Ordering::Relaxed)
    }
}

impl Default for Counting {
    fn default() -> Counting {
        Counting::new()
    }
}

// SAFETY: every call is forwarded to `System` unchanged; the counters only
// observe the sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() && self.on() {
            self.grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() && self.on() {
            self.grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        if self.on() {
            self.shrink(layout.size());
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = System.realloc(ptr, layout, new_size);
        if !new.is_null() && self.on() {
            if new_size >= layout.size() {
                self.grow(new_size - layout.size());
            } else {
                self.shrink(layout.size() - new_size);
            }
        }
        new
    }
}
