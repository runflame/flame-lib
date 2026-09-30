//! The counting allocator, on an instance of its own: the tests' global
//! allocator is the system's, and other tests allocate concurrently.

use std::alloc::{GlobalAlloc, Layout};

use crate::alloc::Counting;

#[test]
fn the_counting_allocator_counts_an_allocation() {
    let counting = Counting::new();
    let layout = Layout::from_size_align(1 << 20, 8).expect("a layout");

    // Before `start`, nothing is counted.
    let early = unsafe { counting.alloc(layout) };
    assert!(!early.is_null());

    counting.start();
    let ptr = unsafe { counting.alloc(layout) };
    assert!(!ptr.is_null());
    unsafe { counting.dealloc(ptr, layout) };
    let small = Layout::from_size_align(1_000, 8).expect("a layout");
    let ptr = unsafe { counting.alloc(small) };
    // Growing it counts the difference; freeing what came before `start`
    // lowers the level, never the peak.
    let ptr = unsafe { counting.realloc(ptr, small, 3_000) };
    unsafe { counting.dealloc(early, layout) };
    assert_eq!(counting.stop(), 1 << 20, "the peak is the 1 MiB allocation");

    // After `stop`, nothing is counted either.
    unsafe { counting.dealloc(ptr, Layout::from_size_align(3_000, 8).expect("a layout")) };
    assert_eq!(counting.stop(), 1 << 20);

    // A new measurement starts from zero.
    counting.start();
    let ptr = unsafe { counting.alloc_zeroed(small) };
    unsafe { counting.dealloc(ptr, small) };
    assert_eq!(counting.stop(), 1_000);
}
