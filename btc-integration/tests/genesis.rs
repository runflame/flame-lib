//! How the node that produces genesis picks the Bitcoin genesis block.

use btc_integration::watcher::impls::MemoryBtc;
use btc_integration::watcher::{GenesisBlockError, GenesisBlockWaiter};
use futures::FutureExt;

#[test]
fn the_wait_ends_when_bitcoin_reaches_the_genesis_height() {
    let btc = MemoryBtc::new();
    let source = btc.source();
    // The node is started ahead of time: Bitcoin is at height 0, the network
    // starts at height 2.
    let mut waiter = GenesisBlockWaiter::new(2);

    assert!(waiter.genesis_id(&source).now_or_never().is_none());
    btc.mine();
    assert!(waiter.genesis_id(&source).now_or_never().is_none());
    let genesis_id = btc.mine();
    assert_eq!(
        waiter.genesis_id(&source).now_or_never(),
        Some(Ok(genesis_id))
    );
}

#[test]
fn a_block_already_at_the_genesis_height_is_taken_at_once() {
    let btc = MemoryBtc::new();
    let genesis_id = btc.mine_n(2);

    let mut waiter = GenesisBlockWaiter::new(2);
    assert_eq!(
        waiter.genesis_id(&btc.source()).now_or_never(),
        Some(Ok(genesis_id))
    );
}

#[test]
fn a_wait_that_begins_too_late_fails() {
    let btc = MemoryBtc::new();
    let best = btc.mine_n(3);
    let source = btc.source();

    let mut waiter = GenesisBlockWaiter::new(2);
    let missed = Some(Err(GenesisBlockError::Missed { height: 2, best }));
    assert_eq!(waiter.genesis_id(&source).now_or_never(), missed);
    // Asking again does not help.
    assert_eq!(waiter.genesis_id(&source).now_or_never(), missed);
}

#[test]
fn blocks_that_arrive_together_do_not_hide_the_genesis_block() {
    let btc = MemoryBtc::new();
    btc.mine();
    let source = btc.source();
    let mut waiter = GenesisBlockWaiter::new(2);
    assert!(waiter.genesis_id(&source).now_or_never().is_none());

    // Bitcoin jumps from height 1 to height 4 between two looks at it.
    btc.mine_n(3);

    assert_eq!(
        waiter.genesis_id(&source).now_or_never(),
        Some(Ok(btc.block_at(2).unwrap()))
    );
}

#[test]
fn a_source_error_does_not_restart_the_wait() {
    let btc = MemoryBtc::new();
    btc.mine();
    let source = btc.source();
    let mut waiter = GenesisBlockWaiter::new(2);
    assert!(waiter.genesis_id(&source).now_or_never().is_none());
    btc.mine_n(2);

    // Source calls: 0 reports the best block (height 3), 1 fetches its header.
    btc.fail_call(1);
    assert!(matches!(
        waiter.genesis_id(&source).now_or_never(),
        Some(Err(GenesisBlockError::Source(_)))
    ));

    // Bitcoin is above the genesis height by now, but the wait began
    // below it: the retry is not "too late".
    assert_eq!(
        waiter.genesis_id(&source).now_or_never(),
        Some(Ok(btc.block_at(2).unwrap()))
    );
}
