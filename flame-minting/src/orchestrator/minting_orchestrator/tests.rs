use std::{
    convert::Infallible,
    sync::atomic::{AtomicBool, Ordering},
};

use btc_integration::{BitcoinNetwork, ShutdownError, StartupError};
use corepc_client::{bitcoin::hashes::Hash, client_sync::Auth};
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

struct StartupProbe(Arc<IdentityManager>, Arc<AtomicBool>);

impl ConsensusIndexer for StartupProbe {
    async fn startup(&self) -> Result<(), StartupError> {
        assert!(self.0.identity().is_some());
        assert!(self.1.load(Ordering::SeqCst));
        Err(StartupError::WorkerStopped)
    }

    async fn shutdown(&self) -> Result<(), ShutdownError> {
        Ok(())
    }
}

struct SenderProbe {
    identity: Arc<IdentityManager>,
    started: Arc<AtomicBool>,
    fail: bool,
}

impl SenderLifecycle for SenderProbe {
    async fn startup(&self) -> Result<(), SenderStartupError> {
        assert!(self.identity.identity().is_some());
        if self.fail {
            return Err(SenderStartupError::Connection(
                btc_integration::BitcoinConnectionError::Rpc(
                    corepc_client::client_sync::Error::Returned("sender setup failed".into()),
                ),
            ));
        }
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn initializes_identity_and_sender_before_starting_indexer() {
    check_startup(false).await;
}

#[tokio::test]
async fn sender_failure_prevents_indexer_and_minter_startup() {
    check_startup(true).await;
}

async fn check_startup(fail_sender: bool) {
    let config = IdentityConfig {
        flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
        access_predicate: Predicate::opaque(Predicate::unspendable_key()),
        validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    };
    let orchestrator = MintingOrchestrator::new(
        Notifier,
        Source,
        Chain,
        BitcoinConfig {
            node_rpc_url: "http://127.0.0.1:1".into(),
            auth: Auth::UserPass("test".into(), "test".into()),
            network: BitcoinNetwork::Regtest,
        },
        BtcBlockTip {
            hash: corepc_client::bitcoin::BlockHash::all_zeros(),
            height: 0,
        },
        config.clone(),
        EndorsementPolicy::Auto,
    )
    .unwrap();
    assert!(orchestrator.identity_manager.identity().is_none());
    assert!(std::ptr::eq(
        orchestrator.identity_manager.as_ref(),
        orchestrator.btc_sender.identity_manager(),
    ));
    assert_eq!(
        orchestrator
            .btc_sender
            .identity_manager()
            .config()
            .validator_pubkey,
        config.validator_pubkey
    );
    let sender_started = Arc::new(AtomicBool::new(false));
    let orchestrator = MintingOrchestrator {
        btc_indexer: Arc::new(StartupProbe(
            orchestrator.identity_manager.clone(),
            sender_started.clone(),
        )),
        btc_sender: SenderProbe {
            identity: orchestrator.identity_manager.clone(),
            started: sender_started.clone(),
            fail: fail_sender,
        },
        identity_manager: orchestrator.identity_manager,
        consensus_manager: orchestrator.consensus_manager,
        minter_manager: orchestrator.minter_manager,
        chain_manager: orchestrator.chain_manager,
        core_block_notifier: orchestrator.core_block_notifier,
    };

    let result = orchestrator.startup().await;
    if fail_sender {
        assert!(matches!(result, Err(OrchestratorError::SenderStartup(_))));
    } else {
        assert!(matches!(
            result,
            Err(OrchestratorError::Consensus(
                consensus::ConsensusLoopError::IndexerStartup(StartupError::WorkerStopped)
            ))
        ));
    }
    assert_eq!(sender_started.load(Ordering::SeqCst), !fail_sender);
    assert!(orchestrator.identity_manager.identity().is_some());
    orchestrator.shutdown().await.unwrap();
}

struct LifecycleIndexer {
    tip: tokio::sync::watch::Sender<Option<BtcBlockTip>>,
    stopped: AtomicBool,
}

impl ConsensusIndexer for LifecycleIndexer {
    async fn startup(&self) -> Result<(), StartupError> {
        Ok(())
    }
    async fn shutdown(&self) -> Result<(), ShutdownError> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<BtcBlockTip>> {
        self.tip.subscribe()
    }
}

struct LifecycleMinter {
    fail_startup: bool,
    fail_shutdown: bool,
}

impl MinterLifecycle for LifecycleMinter {
    async fn startup(
        &self,
        _: tokio::sync::watch::Receiver<Option<BtcBlockTip>>,
    ) -> Result<(), MinterStartupError> {
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

fn lifecycle_orchestrator(
    minter: LifecycleMinter,
) -> MintingOrchestrator<
    SenderProbe,
    Arc<LifecycleIndexer>,
    DefaultConsensusLoop<Chain>,
    LifecycleMinter,
    Chain,
    Notifier,
> {
    let identity_manager = Arc::new(IdentityManager::new(
        Arc::new(InMemorySecretStorage::default()),
        IdentityConfig {
            flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        },
    ));
    MintingOrchestrator {
        btc_sender: SenderProbe {
            identity: identity_manager.clone(),
            started: Arc::new(AtomicBool::new(false)),
            fail: false,
        },
        identity_manager,
        btc_indexer: Arc::new(LifecycleIndexer {
            tip: tokio::sync::watch::channel(None).0,
            stopped: AtomicBool::new(false),
        }),
        consensus_manager: defaults::consensus_loop(
            Chain,
            BtcBlockTip {
                hash: corepc_client::bitcoin::BlockHash::all_zeros(),
                height: 0,
            },
        ),
        minter_manager: minter,
        chain_manager: Chain,
        core_block_notifier: Notifier,
    }
}

#[tokio::test]
async fn minter_startup_failure_shuts_down_the_indexer_and_consensus_worker() {
    let orchestrator = lifecycle_orchestrator(LifecycleMinter {
        fail_startup: true,
        fail_shutdown: false,
    });
    assert!(matches!(
        orchestrator.startup().await,
        Err(OrchestratorError::MinterStartup(
            MinterStartupError::AlreadyRunning
        ))
    ));
    assert!(orchestrator.btc_indexer.stopped.load(Ordering::SeqCst));
    assert_eq!(
        orchestrator.consensus_manager.manager().status(),
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
async fn minter_shutdown_failure_does_not_leave_the_indexer_or_consensus_worker_running() {
    let orchestrator = lifecycle_orchestrator(LifecycleMinter {
        fail_startup: false,
        fail_shutdown: true,
    });
    orchestrator.startup().await.unwrap();
    assert!(matches!(
        orchestrator.shutdown().await,
        Err(OrchestratorError::MinterShutdown(_))
    ));
    assert!(orchestrator.btc_indexer.stopped.load(Ordering::SeqCst));
    assert_eq!(
        orchestrator.consensus_manager.manager().status(),
        crate::ConsensusStatus::Stopped
    );
}
