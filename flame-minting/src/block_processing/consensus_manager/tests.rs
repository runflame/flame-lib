use std::{
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::block_processing::BtcBlockRecords;
use bitcoin::{BlockHash, hashes::Hash};
use flamechain::{BlockHash as FlameHash, BlockTip};
use tokio::sync::Semaphore;

use super::*;
use crate::block_processing::journal::{
    ConsensusJournalEntry, InMemoryBitcoinJournal, InMemoryConsensusJournal,
    InMemoryConsensusJournalError, InMemoryFlameJournal,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Bitcoin(BlockId),
    Flame(BlockTip),
    Rollback(BlockId),
}

struct State {
    cursor: StdMutex<BlockId>,
    calls: StdMutex<Vec<Call>>,
    started: Semaphore,
    gate: Semaphore,
    block_operations: AtomicBool,
    fail_operation: AtomicBool,
    panic_operation: AtomicBool,
    bitcoin_resumes: AtomicUsize,
    flame_resumes: AtomicUsize,
    block_recovery: AtomicBool,
    recovery_started: Semaphore,
    recovery_gate: Semaphore,
    fail_recovery: AtomicBool,
}

impl State {
    async fn operation(&self, call: Call) -> Result<(), &'static str> {
        self.calls.lock().unwrap().push(call);
        self.started.add_permits(1);
        if self.block_operations.load(Ordering::SeqCst) {
            self.gate.acquire().await.unwrap().forget();
        }
        assert!(
            !self.panic_operation.swap(false, Ordering::SeqCst),
            "processor panic"
        );
        if self.fail_operation.swap(false, Ordering::SeqCst) {
            return Err("operation failed");
        }
        Ok(())
    }

    async fn wait_for_calls(&self, count: u32) {
        tokio::time::timeout(Duration::from_secs(5), self.started.acquire_many(count))
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}

struct Bitcoin(Arc<State>);
struct Flame(Arc<State>);

impl BlockProcessor for Bitcoin {
    type Error = &'static str;
    async fn apply(&self, block: BtcBlockRecords) -> Result<BlockId, Self::Error> {
        self.0.operation(Call::Bitcoin(block.id)).await?;
        *self.0.cursor.lock().unwrap() = block.id;
        Ok(block.id)
    }
    async fn resume(&self) -> Result<BlockId, Self::Error> {
        self.0.bitcoin_resumes.fetch_add(1, Ordering::SeqCst);
        self.0.recovery_started.add_permits(1);
        if self.0.block_recovery.load(Ordering::SeqCst) {
            self.0.recovery_gate.acquire().await.unwrap().forget();
        }
        if self.0.fail_recovery.swap(false, Ordering::SeqCst) {
            return Err("recovery failed");
        }
        Ok(*self.0.cursor.lock().unwrap())
    }
    async fn rollback(&self, removed: BlockId) -> Result<BlockId, Self::Error> {
        self.0.operation(Call::Rollback(removed)).await?;
        let previous = tip(removed.height - 1, 1);
        *self.0.cursor.lock().unwrap() = previous;
        Ok(previous)
    }
}

impl FlameProcessor for Flame {
    type Error = &'static str;
    async fn process(&self, tip: BlockTip) -> Result<(), Self::Error> {
        self.0.operation(Call::Flame(tip)).await
    }
    async fn resume(&self) -> Result<(), Self::Error> {
        self.0.flame_resumes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

type Manager = Arc<ConsensusManager<Bitcoin, Flame, InMemoryConsensusJournal>>;

fn fixture(initial: BlockId) -> (Manager, Arc<State>) {
    let state = Arc::new(State {
        cursor: StdMutex::new(initial),
        calls: StdMutex::new(Vec::new()),
        started: Semaphore::new(0),
        gate: Semaphore::new(0),
        block_operations: AtomicBool::new(false),
        fail_operation: AtomicBool::new(false),
        panic_operation: AtomicBool::new(false),
        bitcoin_resumes: AtomicUsize::new(0),
        flame_resumes: AtomicUsize::new(0),
        block_recovery: AtomicBool::new(false),
        recovery_started: Semaphore::new(0),
        recovery_gate: Semaphore::new(0),
        fail_recovery: AtomicBool::new(false),
    });
    (
        Arc::new(ConsensusManager::new(
            Bitcoin(state.clone()),
            Flame(state.clone()),
            InMemoryConsensusJournal::new(initial),
        )),
        state,
    )
}

fn tip(height: u64, branch: u8) -> BlockId {
    BlockId {
        height,
        hash: BlockHash::from_byte_array([branch; 32]),
    }
}
fn flame_tip() -> BlockTip {
    BlockTip {
        hash: FlameHash::new([1; 32]),
        height: 1.into(),
    }
}
fn block(tip: BlockId) -> BtcBlockRecords {
    BtcBlockRecords {
        id: tip,
        burns: vec![],
        endorsements: vec![],
    }
}
fn bitcoin(height: u64) -> ConsensusEvent {
    ConsensusEvent::NewBitcoinBlock(block(tip(height, 1)))
}

#[tokio::test]
async fn handle_rejects_events_before_readiness_and_after_shutdown() {
    let (manager, _) = fixture(tip(1, 1));
    let handle = manager.consensus_handle();
    assert_eq!(
        handle.on_flame_block_arrival(flame_tip()).await,
        Err(ConsensusSendError::NotReady)
    );
    manager.startup().await.unwrap();
    assert!(matches!(
        manager.startup().await,
        Err(ConsensusManagerError::AlreadyRunning)
    ));
    manager.shutdown().await.unwrap();
    assert_eq!(
        handle.on_flame_block_arrival(flame_tip()).await,
        Err(ConsensusSendError::Stopped)
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_finishes_before_the_handle_becomes_ready() {
    let (manager, state) = fixture(tip(1, 1));
    state.block_recovery.store(true, Ordering::SeqCst);
    let startup_manager = manager.clone();
    let startup = tokio::spawn(async move { startup_manager.startup().await });
    state.recovery_started.acquire().await.unwrap().forget();
    assert_eq!(
        manager.enqueue(bitcoin(2)),
        Err(ConsensusSendError::NotReady)
    );
    state.recovery_gate.add_permits(1);
    assert_eq!(startup.await.unwrap().unwrap(), tip(1, 1));
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_recovery_does_not_start_a_worker_and_can_be_retried() {
    let (manager, state) = fixture(tip(1, 1));
    state.fail_recovery.store(true, Ordering::SeqCst);
    assert!(matches!(
        manager.startup().await,
        Err(ConsensusManagerError::Bitcoin("recovery failed"))
    ));
    assert_eq!(
        manager.enqueue(bitcoin(2)),
        Err(ConsensusSendError::NotReady)
    );
    assert!(state.calls.lock().unwrap().is_empty());
    manager.startup().await.unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_during_recovery_prevents_the_queue_from_opening() {
    let (manager, state) = fixture(tip(1, 1));
    state.block_recovery.store(true, Ordering::SeqCst);
    let startup_manager = manager.clone();
    let startup = tokio::spawn(async move { startup_manager.startup().await });
    state.recovery_started.acquire().await.unwrap().forget();
    manager.request_shutdown();
    state.recovery_gate.add_permits(1);
    assert!(matches!(
        startup.await.unwrap(),
        Err(ConsensusManagerError::Delivery(ConsensusSendError::Stopped))
    ));
    manager.shutdown().await.unwrap();
    assert_eq!(manager.status(), ConsensusStatus::Stopped);
    assert!(state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delivery_returns_while_the_operation_is_still_blocked() {
    let (manager, state) = fixture(tip(1, 1));
    state.block_operations.store(true, Ordering::SeqCst);
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    manager.enqueue(bitcoin(2)).unwrap();
    state.wait_for_calls(1).await;
    assert_eq!(*state.cursor.lock().unwrap(), tip(1, 1));
    handle
        .clone()
        .on_flame_block_arrival(flame_tip())
        .await
        .unwrap();
    assert_eq!(state.calls.lock().unwrap().len(), 1);
    manager.request_shutdown();
    state.gate.add_permits(1);
    manager.shutdown().await.unwrap();
    assert_eq!(*state.cursor.lock().unwrap(), tip(2, 1));
    assert_eq!(state.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn bitcoin_and_flame_events_share_fifo_order_without_parallel_execution() {
    let (manager, state) = fixture(tip(1, 1));
    state.block_operations.store(true, Ordering::SeqCst);
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    manager.enqueue(bitcoin(2)).unwrap();
    state.wait_for_calls(1).await;
    handle.on_flame_block_arrival(flame_tip()).await.unwrap();
    manager.enqueue(bitcoin(3)).unwrap();
    assert_eq!(state.calls.lock().unwrap().len(), 1);
    state.gate.add_permits(1);
    state.wait_for_calls(1).await;
    assert_eq!(
        *state.calls.lock().unwrap(),
        vec![Call::Bitcoin(tip(2, 1)), Call::Flame(flame_tip())]
    );
    state.gate.add_permits(1);
    state.wait_for_calls(1).await;
    manager.request_shutdown();
    state.gate.add_permits(1);
    manager.shutdown().await.unwrap();
    assert_eq!(
        *state.calls.lock().unwrap(),
        vec![
            Call::Bitcoin(tip(2, 1)),
            Call::Flame(flame_tip()),
            Call::Bitcoin(tip(3, 1))
        ]
    );
}

#[tokio::test]
async fn reorg_cannot_be_interleaved_and_shutdown_finishes_the_entire_reorg() {
    let (manager, state) = fixture(tip(3, 1));
    state.block_operations.store(true, Ordering::SeqCst);
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    manager
        .enqueue(ConsensusEvent::BitcoinReorg {
            removed_block_tips: vec![tip(3, 1), tip(2, 1)],
            new_blocks: vec![block(tip(2, 2)), block(tip(3, 2))],
        })
        .unwrap();
    state.wait_for_calls(1).await;
    handle.on_flame_block_arrival(flame_tip()).await.unwrap();
    manager.request_shutdown();
    state.gate.add_permits(4);
    manager.shutdown().await.unwrap();
    assert_eq!(
        *state.calls.lock().unwrap(),
        vec![
            Call::Rollback(tip(3, 1)),
            Call::Rollback(tip(2, 1)),
            Call::Bitcoin(tip(2, 2)),
            Call::Bitcoin(tip(3, 2))
        ]
    );
}

#[tokio::test]
async fn reorg_is_one_queue_item_and_following_arrivals_run_after_the_new_branch() {
    let (manager, state) = fixture(tip(3, 1));
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    manager
        .enqueue(ConsensusEvent::BitcoinReorg {
            removed_block_tips: vec![tip(3, 1), tip(2, 1)],
            new_blocks: vec![block(tip(2, 2)), block(tip(3, 2))],
        })
        .unwrap();
    handle.on_flame_block_arrival(flame_tip()).await.unwrap();
    state.wait_for_calls(5).await;
    manager.shutdown().await.unwrap();
    assert_eq!(
        *state.calls.lock().unwrap(),
        vec![
            Call::Rollback(tip(3, 1)),
            Call::Rollback(tip(2, 1)),
            Call::Bitcoin(tip(2, 2)),
            Call::Bitcoin(tip(3, 2)),
            Call::Flame(flame_tip())
        ]
    );
}

#[tokio::test]
async fn operation_failure_stops_the_worker_and_discards_later_events() {
    let (manager, state) = fixture(tip(1, 1));
    state.fail_operation.store(true, Ordering::SeqCst);
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    manager.enqueue(bitcoin(2)).unwrap();
    handle.on_flame_block_arrival(flame_tip()).await.unwrap();
    assert_eq!(manager.stopped().await, ConsensusStatus::Failed);
    assert_eq!(
        manager.enqueue(bitcoin(3)),
        Err(ConsensusSendError::WorkerFailed)
    );
    assert_eq!(state.calls.lock().unwrap().len(), 1);
    assert!(matches!(
        manager.shutdown().await,
        Err(ConsensusManagerError::Bitcoin("operation failed"))
    ));
    manager.startup().await.unwrap();
    handle.on_flame_block_arrival(flame_tip()).await.unwrap();
    state.wait_for_calls(2).await;
    manager.shutdown().await.unwrap();
    assert_eq!(
        *state.calls.lock().unwrap(),
        vec![Call::Bitcoin(tip(2, 1)), Call::Flame(flame_tip())]
    );
}

#[tokio::test]
async fn queue_overflow_terminates_the_process_even_from_a_detached_task() {
    const CHILD: &str = "FLAME_CONSENSUS_QUEUE_OVERFLOW_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "block_processing::consensus_manager::tests::queue_overflow_terminates_the_process_even_from_a_detached_task", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("consensus queue capacity of 1024 exceeded")
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("accepted 1024 waiting events"));
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(output.status.signal(), Some(6));
        }
        #[cfg(not(unix))]
        assert!(!output.status.success());
        return;
    }
    let (manager, state) = fixture(tip(1, 1));
    state.block_operations.store(true, Ordering::SeqCst);
    manager.startup().await.unwrap();
    manager.enqueue(bitcoin(2)).unwrap();
    state.wait_for_calls(1).await;
    for _ in 0..1024 {
        manager.enqueue(bitcoin(3)).unwrap();
    }
    eprintln!("accepted 1024 waiting events");
    tokio::spawn(async move { manager.enqueue(bitcoin(3)) });
    tokio::time::sleep(Duration::from_secs(1)).await;
    panic!("queue overflow did not terminate the process");
}

#[tokio::test]
async fn restart_reuses_existing_handles_without_replaying_the_old_queue() {
    let (manager, state) = fixture(tip(1, 1));
    manager.startup().await.unwrap();
    let handle = manager.consensus_handle();
    manager.enqueue(bitcoin(2)).unwrap();
    manager.shutdown().await.unwrap();
    assert!(state.calls.lock().unwrap().is_empty());
    manager.startup().await.unwrap();
    handle
        .clone()
        .on_flame_block_arrival(flame_tip())
        .await
        .unwrap();
    state.wait_for_calls(1).await;
    manager.shutdown().await.unwrap();
    assert_eq!(*state.calls.lock().unwrap(), vec![Call::Flame(flame_tip())]);
    assert_eq!(state.bitcoin_resumes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn worker_panics_are_rethrown_when_joined() {
    let (manager, state) = fixture(tip(1, 1));
    state.panic_operation.store(true, Ordering::SeqCst);
    manager.startup().await.unwrap();
    manager.enqueue(bitcoin(2)).unwrap();
    assert_eq!(manager.stopped().await, ConsensusStatus::Failed);
    let shutdown = tokio::spawn(async move { manager.shutdown().await });
    assert!(shutdown.await.unwrap_err().is_panic());
}

#[tokio::test]
async fn public_handle_keeps_the_manager_alive_and_delegates_flame_arrivals() {
    let (manager, state) = fixture(tip(1, 1));
    manager.startup().await.unwrap();
    let weak = Arc::downgrade(&manager);
    let handle = manager.consensus_handle();
    drop(manager);
    handle
        .clone()
        .on_flame_block_arrival(flame_tip())
        .await
        .unwrap();
    state.wait_for_calls(1).await;
    let manager = weak.upgrade().unwrap();
    manager.shutdown().await.unwrap();
    drop(manager);
    drop(handle);
    assert!(weak.upgrade().is_none());
    assert_eq!(*state.calls.lock().unwrap(), vec![Call::Flame(flame_tip())]);
}

struct UnreadableJournal(InMemoryConsensusJournal);

impl ConsensusJournal for UnreadableJournal {
    type Error = InMemoryConsensusJournalError;
    type Bitcoin = InMemoryBitcoinJournal;
    type Flame = InMemoryFlameJournal;
    fn bitcoin_journal(&self) -> Self::Bitcoin {
        self.0.bitcoin_journal()
    }
    fn flame_journal(&self) -> Self::Flame {
        self.0.flame_journal()
    }
    async fn latest_entry(&self) -> Result<Option<ConsensusJournalEntry>, Self::Error> {
        Err(InMemoryConsensusJournalError::LockPoisoned)
    }
}

#[tokio::test]
async fn journal_read_failure_prevents_recovery_and_event_acceptance() {
    let (_, state) = fixture(tip(1, 1));
    let manager = ConsensusManager::new(
        Bitcoin(state.clone()),
        Flame(state.clone()),
        UnreadableJournal(InMemoryConsensusJournal::new(tip(1, 1))),
    );
    assert!(matches!(
        manager.startup().await,
        Err(ConsensusManagerError::Journal(
            InMemoryConsensusJournalError::LockPoisoned
        ))
    ));
    assert_eq!(state.bitcoin_resumes.load(Ordering::SeqCst), 0);
    assert_eq!(state.flame_resumes.load(Ordering::SeqCst), 0);
    assert_eq!(manager.status(), ConsensusStatus::NotReady);
}
