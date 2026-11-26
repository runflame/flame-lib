use std::{future::Future, panic::resume_unwind, sync::Arc, time::Duration};

use bitcoin::{Amount, TxOut, Txid};
use bitcoind::anyhow::{Context, Result, anyhow};
use btc_integration::{
    BtcWallet, BurnData, IdentityConfig, MinterIdentity, MinterWallet, MintingEndorsementData,
    identity::impls::LocalKey,
    wallet::impls::RpcWatchOnlyWallet,
    watcher::{BlockId, impls::RpcSource},
};
use flame_chain_service::InMemoryChain;
use flame_minting::{
    MintingOrchestrator,
    minter::{EndorsementPolicy, MinterManager, journal::in_memory::InMemoryMinterJournal},
    orchestrator::{DefaultConsensusLoop, cursor_storage::CursorStorage, sender::MintingSender},
};
use flamechain::BlockHash;
use flamevm::Predicate;
use tokio::time::{sleep, timeout};

use super::{BitcoinRegtest, FlameChain, UnusedNotifier};

type TestOrchestrator = MintingOrchestrator<
    Arc<MintingSender>,
    Arc<RpcSource>,
    DefaultConsensusLoop<InMemoryChain>,
    MinterManager<InMemoryChain, MintingSender, InMemoryMinterJournal>,
    InMemoryChain,
    UnusedNotifier,
>;

/// What every minter of a test starts with at its witness script.
const MINTER_FUNDING: Amount = Amount::from_sat(50_000);

pub struct TestContext {
    pub bitcoin: Arc<BitcoinRegtest>,
    pub orchestrator: Arc<TestOrchestrator>,
    pub flame_chain: FlameChain,
    pub identity: MinterIdentity,
    identity_config: IdentityConfig,
}

/// A minter next to the one of the orchestrator: the test burns and endorses
/// for it by hand.
pub struct TestMinter {
    bitcoin: Arc<BitcoinRegtest>,
    burn: BurnData,
    wallet: MinterWallet<RpcWatchOnlyWallet, LocalKey>,
}

impl TestMinter {
    pub async fn send_acquisition(&self, amount: Amount) -> Result<Txid> {
        Ok(self
            .bitcoin
            .wallet
            .send(self.burn.to_output(amount))
            .await?)
    }

    pub async fn send_endorsement(&self, height: u32, hash: BlockHash) -> Result<Txid> {
        let endorsement = MintingEndorsementData::V1 {
            flame_block_height: height,
            flame_block_hash: hash,
        };
        Ok(self.wallet.send(endorsement.to_output()).await?)
    }
}

/// Opens the watch-only wallet of the minter, then pays [`MINTER_FUNDING`] to
/// its witness script. In this order: the wallet watches the script from the
/// moment it is opened.
async fn fund_minter(
    bitcoin: &BitcoinRegtest,
    identity: &MinterIdentity,
) -> Result<RpcWatchOnlyWallet> {
    let script = identity.witness_script();
    let watch_only = RpcWatchOnlyWallet::open(&bitcoin.bitcoin_config(), script).await?;
    let funding = TxOut {
        value: MINTER_FUNDING,
        script_pubkey: script.to_p2wsh(),
    };
    bitcoin.wallet.send(funding).await?;
    Ok(watch_only)
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
            bitcoin.bitcoin_config(),
            initial_cursor,
            identity_config.clone(),
            LocalKey::generate(),
            EndorsementPolicy::Auto,
        )?);
        let identity = orchestrator.identity.clone();
        // The sender of the orchestrator opens the same wallet when it starts.
        fund_minter(&bitcoin, &identity).await?;
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
            identity_config,
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
        let burn = self.burn_for(&self.identity);
        Ok(self.bitcoin.wallet.send(burn.to_output(amount)).await?)
    }

    fn burn_for(&self, identity: &MinterIdentity) -> BurnData {
        BurnData::new(
            identity.p2wsh(),
            self.identity_config.access_predicate.clone(),
        )
    }

    /// One more minter with the same configuration and a key of its own.
    pub async fn create_minter(&self) -> Result<TestMinter> {
        let key = LocalKey::generate();
        let identity = self.identity_config.identity(&key);
        let watch_only = fund_minter(&self.bitcoin, &identity).await?;
        Ok(TestMinter {
            bitcoin: self.bitcoin.clone(),
            burn: self.burn_for(&identity),
            wallet: MinterWallet::new(watch_only, key, identity)?,
        })
    }

    pub async fn start(&self) -> Result<()> {
        self.orchestrator
            .startup()
            .await
            .map_err(|error| anyhow!("orchestrator startup: {error:?}"))
    }

    pub async fn wait_for_processing(&self, expected: BlockId) -> Result<()> {
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
