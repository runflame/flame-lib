use std::{
    convert::Infallible,
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};

use btc_integration::{HistoryError, HistoryUpdate, ShutdownError, StartupError};
use corepc_client::{bitcoin::hashes::Hash, client_sync::Auth};
use flame_chain_service::{ChainPath, ChangesOutcome, ImportOutcome};
use flamechain::{Block, BlockHash, BlockTip};
use flamevm::Predicate;
use tokio::sync::watch;

use super::*;

#[derive(Clone)]
struct Chain;

impl ChainAccess for Chain {
    type Error = Infallible;

    async fn set_as_child(&self, _: BlockTip, _: BlockTip) -> Result<(), Self::Error> {
        unreachable!()
    }

    async fn get_block(&self, _: BlockTip) -> Result<Option<Arc<Block>>, Self::Error> {
        unreachable!()
    }

    async fn get_chain_path(&self, _: BlockTip, _: BlockTip) -> Result<ChainPath, Self::Error> {
        unreachable!()
    }

    async fn import_block(&mut self, _: Arc<Block>) -> Result<ImportOutcome, Self::Error> {
        unreachable!()
    }

    async fn select_tip(&mut self, _: BlockHash) -> Result<ChangesOutcome, Self::Error> {
        unreachable!()
    }
}

struct Source;

impl CoreBlockSource for Source {
    type Error = Infallible;

    async fn get_core_block(&self, _: u64) -> Result<Option<Arc<Block>>, Self::Error> {
        unreachable!()
    }

    async fn wait_core_block(&self, _: u64) -> Result<Option<Arc<Block>>, Self::Error> {
        unreachable!()
    }
}

struct Notifier;

impl CoreBlockNotifier for Notifier {
    type Error = Infallible;

    async fn notify_core_block_needed(&mut self, _: u64) -> Result<(), Self::Error> {
        unreachable!()
    }
}

struct StartupProbe(Arc<IdentityManager>, Arc<AtomicBool>);

impl ports::ConsensusIndexer for StartupProbe {
    async fn startup(&self) -> Result<(), StartupError> {
        assert!(self.0.identity().is_some());
        assert!(self.1.load(Ordering::SeqCst));
        Err(StartupError::WorkerStopped)
    }

    async fn shutdown(&self) -> Result<(), ShutdownError> {
        Ok(())
    }

    fn subscribe(&self) -> watch::Receiver<Option<BtcBlockTip>> {
        unreachable!()
    }

    async fn get_history(
        &self,
        _: BtcBlockTip,
        _: NonZeroUsize,
    ) -> Result<HistoryUpdate, HistoryError> {
        unreachable!()
    }
}

struct SenderProbe {
    identity: Arc<IdentityManager>,
    started: Arc<AtomicBool>,
    fail: bool,
}

impl ports::SenderLifecycle for SenderProbe {
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
        },
        BtcBlockTip {
            hash: corepc_client::bitcoin::BlockHash::all_zeros(),
            height: 0,
        },
        config.clone(),
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
