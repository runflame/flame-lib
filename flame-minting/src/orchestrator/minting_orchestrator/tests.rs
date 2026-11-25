use std::{
    convert::Infallible,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};

use btc_integration::{
    BitcoinRpcAuth, WalletError,
    watcher::impls::{MemoryBtc, MemorySource},
};
use flamevm::Predicate;

use super::*;

#[derive(Clone)]
struct Chain;

impl ChainAccess for Chain {
    type Error = Infallible;
}

struct Source;

impl CoreBlockSource for Source {
    type Error = Infallible;
}

struct Notifier;

impl CoreBlockNotifier for Notifier {
    type Error = Infallible;
}

fn identity_config() -> IdentityConfig {
    IdentityConfig {
        flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
        access_predicate: Predicate::opaque(Predicate::unspendable_key()),
        validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    }
}

struct SenderProbe {
    started: Arc<AtomicBool>,
    fail: bool,
}

impl SenderLifecycle for SenderProbe {
    async fn startup(&self) -> Result<(), SenderStartupError> {
        if self.fail {
            let failure = WalletError::Unreachable("sender setup failed".into());
            return Err(SenderStartupError(failure));
        }
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct LifecycleMinter {
    /// What the minter saw when it was started: whether the sender had been
    /// started, and the best Bitcoin block consensus had published.
    seen_at_startup: StdMutex<Option<(bool, Option<BlockId>)>>,
    sender_started: Arc<AtomicBool>,
    fail_startup: bool,
    fail_shutdown: bool,
}

impl MinterLifecycle for LifecycleMinter {
    async fn startup(
        &self,
        subscription: tokio::sync::watch::Receiver<Option<BlockId>>,
    ) -> Result<(), MinterStartupError> {
        let sender_started = self.sender_started.load(Ordering::SeqCst);
        *self.seen_at_startup.lock().unwrap() = Some((sender_started, *subscription.borrow()));
        if self.fail_startup {
            Err(MinterStartupError::AlreadyRunning)
        } else {
            Ok(())
        }
    }
    async fn shutdown(&self) -> Result<(), MinterShutdownError> {
        if self.fail_shutdown {
            let error = tokio::spawn(async { panic!("minter failure") })
                .await
                .unwrap_err();
            return Err(MinterShutdownError::WorkerJoin(error));
        }
        Ok(())
    }
}

type TestOrchestrator = MintingOrchestrator<
    SenderProbe,
    Arc<MemorySource>,
    DefaultConsensusLoop<Chain>,
    LifecycleMinter,
    Chain,
    Notifier,
>;

#[derive(Default)]
struct Failures {
    sender: bool,
    minter_startup: bool,
    minter_shutdown: bool,
}

/// An orchestrator over Bitcoin in memory, with consensus at its best block.
fn orchestrator(btc: &MemoryBtc, failures: Failures) -> TestOrchestrator {
    let sender_started = Arc::new(AtomicBool::new(false));
    MintingOrchestrator {
        identity: identity_config().identity(&LocalKey::generate()),
        btc_sender: SenderProbe {
            started: sender_started.clone(),
            fail: failures.sender,
        },
        btc_source: Arc::new(btc.source()),
        consensus_manager: defaults::consensus_loop(Chain, btc.best()),
        minter_manager: LifecycleMinter {
            seen_at_startup: StdMutex::new(None),
            sender_started,
            fail_startup: failures.minter_startup,
            fail_shutdown: failures.minter_shutdown,
        },
        chain_manager: Chain,
        core_block_notifier: Notifier,
    }
}

fn consensus_status(orchestrator: &TestOrchestrator) -> crate::ConsensusStatus {
    orchestrator.consensus_manager.manager().status()
}

#[test]
fn the_sender_signs_for_the_identity_of_the_key() {
    let key = LocalKey::generate();
    let identity = identity_config().identity(&key);
    let orchestrator = MintingOrchestrator::new(
        Notifier,
        Source,
        Chain,
        BitcoinConfig {
            node_rpc_url: "http://127.0.0.1:1".into(),
            auth: BitcoinRpcAuth::UserPass("test".into(), "test".into()),
        },
        MemoryBtc::new().best(),
        identity_config(),
        key,
        EndorsementPolicy::Auto,
    )
    .unwrap();
    assert_eq!(orchestrator.identity, identity);
    assert_eq!(orchestrator.btc_sender.identity(), &identity);
}

#[tokio::test]
async fn starts_the_sender_then_consensus_then_the_minter() {
    let btc = MemoryBtc::new();
    let orchestrator = orchestrator(&btc, Failures::default());

    orchestrator.startup().await.unwrap();

    let seen = *orchestrator.minter_manager.seen_at_startup.lock().unwrap();
    // Bitcoin has no new block: the minter has not heard of one yet.
    assert_eq!(seen, Some((true, None)));
    assert_eq!(
        consensus_status(&orchestrator),
        crate::ConsensusStatus::Running
    );
    orchestrator.shutdown().await.unwrap();
}

#[tokio::test]
async fn sender_failure_prevents_consensus_and_minter_startup() {
    let orchestrator = orchestrator(
        &MemoryBtc::new(),
        Failures {
            sender: true,
            ..Failures::default()
        },
    );

    assert!(matches!(
        orchestrator.startup().await,
        Err(OrchestratorError::SenderStartup(_))
    ));

    assert_eq!(
        consensus_status(&orchestrator),
        crate::ConsensusStatus::NotReady
    );
    let seen = *orchestrator.minter_manager.seen_at_startup.lock().unwrap();
    assert_eq!(seen, None);
    orchestrator.shutdown().await.unwrap();
}

#[tokio::test]
async fn minter_startup_failure_shuts_down_the_consensus_worker() {
    let orchestrator = orchestrator(
        &MemoryBtc::new(),
        Failures {
            minter_startup: true,
            ..Failures::default()
        },
    );
    assert!(matches!(
        orchestrator.startup().await,
        Err(OrchestratorError::MinterStartup(
            MinterStartupError::AlreadyRunning
        ))
    ));
    assert_eq!(
        consensus_status(&orchestrator),
        crate::ConsensusStatus::Stopped
    );
    assert_eq!(
        orchestrator
            .consensus_handle()
            .on_flame_block_arrival(flamechain::BlockTip {
                hash: flamechain::BlockHash::from([1; 32]),
                height: 1.into()
            })
            .await,
        Err(crate::ConsensusSendError::Stopped)
    );
}

#[tokio::test]
async fn minter_shutdown_failure_does_not_leave_the_consensus_worker_running() {
    let orchestrator = orchestrator(
        &MemoryBtc::new(),
        Failures {
            minter_shutdown: true,
            ..Failures::default()
        },
    );
    orchestrator.startup().await.unwrap();
    assert!(matches!(
        orchestrator.shutdown().await,
        Err(OrchestratorError::MinterShutdown(_))
    ));
    assert_eq!(
        consensus_status(&orchestrator),
        crate::ConsensusStatus::Stopped
    );
}
