mod bitcoin;
mod flame;
mod minter;
mod runtime;
mod snapshot;

use std::{sync::Arc, time::Duration};

use ::bitcoin::Amount;
use anyhow::{Context, Result, anyhow};
use btc_integration::{IdentityConfig, identity::impls::LocalKey, watcher::BlockId};
use flame_chain_service::InMemoryChain;
use flame_minting::orchestrator::cursor_storage::CursorStorage;
use flame_storage::CanonicalStorage;
use flamevm::Predicate;
use tokio::time::{sleep, timeout};

use crate::{
    error::DemoError,
    types::{
        BitcoinBlockHash, BitcoinBlockTip, BitcoinTxid, CreateBitcoinBlocksRequest,
        CreateFlameBlockRequest, CreateMinterRequest, FlameBlockHash, FlameBlockSnapshot,
        FlameBlockTip, MinterId, MinterSnapshot, SendAcquisitionRequest, SendEndorsementRequest,
        SubmittedTransaction,
    },
};

use bitcoin::BitcoinRegtest;
use flame::{DemoFlameChain, parse_flame_hash};
use minter::DemoMinter;

use runtime::DemoRuntime;

pub struct DemoSession {
    runtime: DemoRuntime,
    /// What every minter of the session shares; each has a key of its own.
    identity_config: IdentityConfig,
    flame: DemoFlameChain,
    minters: Vec<DemoMinter>,
    bitcoin: BitcoinRegtest,
}

impl DemoSession {
    pub async fn start() -> Result<Self> {
        let bitcoin = BitcoinRegtest::start()
            .await
            .context("start Bitcoin regtest")?;
        let chain = InMemoryChain::default();
        let identity_config = IdentityConfig {
            flame_predicate: Predicate::opaque(Predicate::unspendable_key()),
            access_predicate: Predicate::opaque(Predicate::unspendable_key()),
            validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
        };
        // The first minter is the one of the node: both sign with one key.
        let identity_key = Arc::new(LocalKey::generate());
        let runtime = DemoRuntime::new(
            chain.clone(),
            bitcoin.config(),
            bitcoin.tip().await?,
            &identity_config,
            identity_key.clone(),
        )?;
        let minter = DemoMinter::create(
            MinterId(0),
            "Alice".into(),
            &identity_config,
            identity_key,
            &bitcoin,
        )
        .await?;
        bitcoin.mine_block().await?;
        minter
            .send_acquisition(&bitcoin, Amount::from_sat(20_000))
            .await?;
        let acquisition_tip = bitcoin.mine_block().await?;
        let target_btc_height = acquisition_tip
            .height
            .checked_add(u64::from(runtime.parameters.acquisition_maturity))
            .context("genesis target Bitcoin height overflow")?;
        let flame = DemoFlameChain::new(
            chain,
            runtime.blocks.clone(),
            runtime.consensus.clone(),
            runtime.canonical.clone(),
            u32::try_from(target_btc_height).context("genesis target exceeds u32")?,
        )
        .await?;
        let mut session = Self {
            runtime,
            identity_config,
            flame,
            minters: vec![minter],
            bitcoin,
        };
        let startup = async {
            while session.bitcoin.tip().await?.height + 1 < target_btc_height {
                session.bitcoin.mine_block().await?;
            }
            let genesis_tip = session.canonical_tip().await?;
            let genesis = session.flame_block(&genesis_tip.hash).await?;
            session
                .send_endorsement(SendEndorsementRequest {
                    minter_id: MinterId(0),
                    core_height: genesis.core.context("genesis is not a core block")?.height,
                    block_hash: genesis_tip.hash,
                })
                .await?;
            let endorsed_tip = session.bitcoin.mine_block().await?;
            session
                .runtime
                .orchestrator
                .startup()
                .await
                .map_err(|error| anyhow!("start orchestrator: {error:?}"))?;
            session.wait_for_processing(endorsed_tip).await
        };
        match timeout(Duration::from_secs(30), startup)
            .await
            .context("session startup timed out")
            .and_then(|result| result)
        {
            Ok(()) => Ok(session),
            Err(error) => match session.shutdown().await {
                Ok(()) => Err(error),
                Err(shutdown) => {
                    Err(error.context(format!("startup cleanup failed: {shutdown:#}")))
                }
            },
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        let result = timeout(
            Duration::from_secs(20),
            self.runtime.orchestrator.shutdown(),
        )
        .await
        .context("orchestrator shutdown timed out")
        .and_then(|result| result.map_err(|error| anyhow!("stop orchestrator: {error:?}")));
        tokio::task::spawn_blocking(move || drop(self)).await?;
        result
    }

    pub async fn create_bitcoin_blocks(
        &mut self,
        request: CreateBitcoinBlocksRequest,
    ) -> Result<Vec<BitcoinBlockTip>> {
        if request.count.get() > 100 {
            return Err(
                DemoError::InvalidRequest("mine at most 100 blocks per request".into()).into(),
            );
        }
        let mut blocks = Vec::new();
        for _ in 0..request.count.get() {
            let tip = self.bitcoin.mine_block().await?;
            self.wait_for_processing(tip).await?;
            blocks.push(bitcoin_tip(tip));
        }
        Ok(blocks)
    }

    pub async fn create_flame_block(
        &mut self,
        request: CreateFlameBlockRequest,
    ) -> Result<FlameBlockSnapshot> {
        self.wait_for_processing(self.bitcoin.tip().await?).await?;
        self.flame.create_block(request).await
    }

    pub async fn create_minter(&mut self, request: CreateMinterRequest) -> Result<MinterSnapshot> {
        let name = request.name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(DemoError::InvalidRequest(
                "minter name must contain 1 to 80 characters".into(),
            )
            .into());
        }
        let id = MinterId(u32::try_from(self.minters.len()).context("too many minters")?);
        let key = Arc::new(LocalKey::generate());
        let config = &self.identity_config;
        let minter = DemoMinter::create(id, name.into(), config, key, &self.bitcoin).await?;
        self.minters.push(minter);
        self.minter(id)?.snapshot(&self.runtime.consensus).await
    }

    pub async fn send_acquisition(
        &mut self,
        request: SendAcquisitionRequest,
    ) -> Result<SubmittedTransaction> {
        let amount = Amount::from_sat(request.amount_sats.get());
        let minter = self.minter(request.minter_id)?;
        let txid = minter.send_acquisition(&self.bitcoin, amount).await?;
        Ok(SubmittedTransaction {
            txid: BitcoinTxid(txid.to_string()),
        })
    }

    pub async fn send_endorsement(
        &mut self,
        request: SendEndorsementRequest,
    ) -> Result<SubmittedTransaction> {
        let hash = parse_flame_hash(&request.block_hash)?;
        let txid = self
            .minter(request.minter_id)?
            .send_endorsement(request.core_height, hash)
            .await?;
        Ok(SubmittedTransaction {
            txid: BitcoinTxid(txid.to_string()),
        })
    }

    pub async fn bitcoin_tip(&self) -> Result<BitcoinBlockTip> {
        Ok(bitcoin_tip(self.bitcoin.tip().await?))
    }

    pub async fn bitcoin_mempool(&self) -> Result<Vec<BitcoinTxid>> {
        self.bitcoin.mempool().await
    }

    pub async fn consensus_cursor(&self) -> Result<BitcoinBlockTip> {
        Ok(bitcoin_tip(self.runtime.cursor.get_cursor().await?))
    }

    pub async fn canonical_tip(&self) -> Result<FlameBlockTip> {
        let tip = self
            .runtime
            .canonical
            .get_tip()
            .await?
            .context("missing canonical tip")?;
        Ok(flame_tip(tip))
    }

    pub async fn flame_block(&self, hash: &FlameBlockHash) -> Result<FlameBlockSnapshot> {
        self.flame.snapshot(parse_flame_hash(hash)?).await
    }

    pub async fn minters(&self) -> Result<Vec<MinterSnapshot>> {
        let mut minters = Vec::with_capacity(self.minters.len());
        for minter in &self.minters {
            minters.push(minter.snapshot(&self.runtime.consensus).await?);
        }
        Ok(minters)
    }

    fn minter(&self, id: MinterId) -> Result<&DemoMinter> {
        self.minters
            .get(id.0 as usize)
            .ok_or_else(|| DemoError::NotFound(format!("unknown minter {}", id.0)).into())
    }

    async fn wait_for_processing(&self, expected: BlockId) -> Result<()> {
        timeout(Duration::from_secs(10), async {
            loop {
                if self.runtime.cursor.get_cursor().await? == expected {
                    return Ok(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| {
            DemoError::Timeout(format!("consensus did not reach BTC block {expected:?}"))
        })?
    }
}

fn bitcoin_tip(tip: BlockId) -> BitcoinBlockTip {
    BitcoinBlockTip {
        hash: BitcoinBlockHash(tip.hash.to_string()),
        height: tip.height,
    }
}

fn flame_tip(tip: flamechain::BlockTip) -> FlameBlockTip {
    FlameBlockTip {
        hash: FlameBlockHash(hex::encode(tip.hash.as_bytes())),
        height: tip.height.as_u64(),
    }
}
