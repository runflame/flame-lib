use std::{future::Future, panic::resume_unwind, sync::Arc, time::Duration};

use bitcoind::anyhow::{Context, Result, anyhow};
use btc_integration::{
    BitcoinConnection, BtcBlockTip, IdentityConfig, IdentityManager, MinterIdentity,
    ProtocolIndexerV31, TestAcquisitionConfig, TestSender, btc::rpc::Core31RpcApi,
    identity::storage::InMemorySecretStorage,
};
use corepc_client::bitcoin::{Address, Amount, Network, Txid};
use flame_chain_service::InMemoryChain;
use flame_minting::{
    MintingOrchestrator,
    minter::{MinterManager, journal::in_memory::InMemoryMinterJournal},
    orchestrator::{DefaultConsensusLoop, cursor_storage::CursorStorage, sender::MintingSender},
};
use flamevm::Predicate;
use tokio::time::{sleep, timeout};

use super::{BitcoinRegtest, FlameChain, UnusedNotifier};

type TestOrchestrator = MintingOrchestrator<
    Arc<MintingSender>,
    Arc<ProtocolIndexerV31>,
    DefaultConsensusLoop<InMemoryChain>,
    MinterManager<InMemoryChain, MintingSender, InMemoryMinterJournal>,
    InMemoryChain,
    UnusedNotifier,
>;

pub struct TestContext {
    pub bitcoin: Arc<BitcoinRegtest>,
    pub orchestrator: Arc<TestOrchestrator>,
    pub flame_chain: FlameChain,
    pub identity: MinterIdentity,
    acquisition_sender: Arc<TestSender<Core31RpcApi, Arc<InMemorySecretStorage>>>,
}

impl TestContext {
    async fn new() -> Result<Self> {
        let bitcoin = Arc::new(BitcoinRegtest::new()?);
        let initial_cursor = bitcoin.mine_block().await?;
        let chain = InMemoryChain::default();
        let identity_config = IdentityConfig {
            flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        };
        let orchestrator = Arc::new(MintingOrchestrator::new(
            UnusedNotifier,
            chain.clone(),
            chain.clone(),
            bitcoin.config(),
            initial_cursor,
            identity_config.clone(),
        )?);
        let identity = orchestrator.identity_manager.startup().await?.clone();
        let connection = BitcoinConnection::regtest(
            bitcoin.config(),
            identity.clone(),
            orchestrator.identity_manager.secret_storage().clone(),
            TestAcquisitionConfig {
                wallet_rpc_url: bitcoin.node.rpc_url_with_wallet("default"),
                access_predicate: identity_config.access_predicate,
                validator_pubkey: identity_config.validator_pubkey,
            },
        )
        .await?;
        let funding_address = Address::p2wsh(identity.witness_script(), Network::Regtest);
        bitcoin
            .node
            .client
            .send_to_address(&funding_address, Amount::from_sat(50_000))?;
        let flame_chain = FlameChain::new(
            chain,
            orchestrator.get_chain_storage().clone(),
            orchestrator.get_consensus_storage().clone(),
            orchestrator.get_canonical_storage(),
        )
        .await?;
        Ok(Self {
            bitcoin,
            orchestrator,
            flame_chain,
            identity,
            acquisition_sender: connection.get_sender(),
        })
    }

    pub async fn run<F, Fut>(scenario: F) -> Result<()>
    where
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let context = Self::new().await?;
        let orchestrator = context.orchestrator.clone();
        let bitcoin = context.bitcoin.clone();
        run_with_cleanup(async move { scenario(context).await }, async move {
            let result = timeout(Duration::from_secs(20), orchestrator.shutdown())
                .await
                .context("timed out shutting down the orchestrator")?
                .map_err(|error| anyhow!("orchestrator shutdown: {error:?}"));
            drop(orchestrator);
            drop(bitcoin);
            result
        })
        .await
    }

    pub async fn send_acquisition(&self, amount: Amount) -> Result<Txid> {
        Ok(self.acquisition_sender.send_acquisition(amount).await?)
    }

    pub async fn create_minter(
        &self,
    ) -> Result<Arc<TestSender<Core31RpcApi, Arc<InMemorySecretStorage>>>> {
        let config = self.orchestrator.identity_manager.config().clone();
        let manager =
            IdentityManager::new(Arc::new(InMemorySecretStorage::default()), config.clone());
        let identity = manager.startup().await?.clone();
        let connection = BitcoinConnection::regtest(
            self.bitcoin.config(),
            identity.clone(),
            manager.secret_storage().clone(),
            TestAcquisitionConfig {
                wallet_rpc_url: self.bitcoin.node.rpc_url_with_wallet("default"),
                access_predicate: config.access_predicate,
                validator_pubkey: config.validator_pubkey,
            },
        )
        .await?;
        let address = Address::p2wsh(identity.witness_script(), Network::Regtest);
        self.bitcoin
            .node
            .client
            .send_to_address(&address, Amount::from_sat(50_000))?;
        Ok(connection.get_sender())
    }

    pub async fn start(&self) -> Result<()> {
        self.orchestrator
            .startup()
            .await
            .map_err(|error| anyhow!("orchestrator startup: {error:?}"))
    }

    pub async fn wait_for_processing(&self, expected: BtcBlockTip) -> Result<()> {
        let mut last_cursor = None;
        timeout(Duration::from_secs(10), async {
            loop {
                let cursor = self.orchestrator.get_cursor_storage().get_cursor().await?;
                last_cursor = Some(cursor);
                if cursor == expected {
                    return Ok(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .with_context(|| {
            format!("timed out waiting for consensus cursor {expected:?}; last processed cursor: {last_cursor:?}")
        })?
    }
}

async fn run_with_cleanup<S, C>(scenario: S, cleanup: C) -> Result<()>
where
    S: Future<Output = Result<()>> + Send + 'static,
    C: Future<Output = Result<()>>,
{
    let outcome = tokio::spawn(scenario).await;
    let shutdown = cleanup.await;
    let result = match outcome {
        Ok(result) => result,
        Err(error) if error.is_panic() => {
            if let Err(shutdown) = shutdown {
                eprintln!("cleanup after scenario panic failed: {shutdown:#}");
            }
            resume_unwind(error.into_panic());
        }
        Err(error) => Err(error.into()),
    };
    match (result, shutdown) {
        (Ok(()), shutdown) => shutdown,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(shutdown)) => {
            Err(error.context(format!("cleanup also failed: {shutdown:#}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn cleanup_finishes_before_the_original_panic_is_propagated() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup_finished = cleaned.clone();
        let task = tokio::spawn(run_with_cleanup(
            async { panic!("scenario assertion failed") },
            async move {
                tokio::task::yield_now().await;
                cleanup_finished.store(true, Ordering::SeqCst);
                Ok(())
            },
        ));
        let panic = task.await.unwrap_err().into_panic();
        assert!(cleaned.load(Ordering::SeqCst));
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"scenario assertion failed")
        );
    }

    #[tokio::test]
    async fn cleanup_failure_does_not_discard_the_scenario_error() {
        let error = run_with_cleanup(async { Err(anyhow!("scenario failed")) }, async {
            Err(anyhow!("shutdown failed"))
        })
        .await
        .unwrap_err();
        assert_eq!(error.root_cause().to_string(), "scenario failed");
        assert!(error.to_string().contains("shutdown failed"));
    }
}
