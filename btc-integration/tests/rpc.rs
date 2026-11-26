mod common;

use bitcoin::hashes::Hash;
use bitcoin::secp256k1::SecretKey;
use bitcoin::{Address, Amount, Network, ScriptBuf, TxOut};
use bitcoind::anyhow::Context;
use btc_integration::identity::impls::LocalKey;
use btc_integration::protocol::{BtcBlockRecords, BurnData, MinterP2wsh, MintingEndorsementData};
use btc_integration::wallet::impls::{RpcWallet, RpcWatchOnlyWallet};
use btc_integration::watcher::{BlockId, ChainSource};
use btc_integration::{
    BitcoinConfig, BitcoinRpcAuth, BtcWallet, IdentityConfig, MinterWallet, WalletError,
};
use common::{TestContext, setup};
use flamechain::BlockHash;
use flamevm::Predicate;

/// The wallet of a minter whose script has a confirmed output of `funding`.
async fn minter_wallet(
    ctx: &TestContext,
    funding: Amount,
) -> bitcoind::anyhow::Result<MinterWallet<RpcWatchOnlyWallet, LocalKey>> {
    let key = LocalKey::new(SecretKey::from_slice(&[0x41; 32])?);
    let nobody = Predicate::opaque(Predicate::unspendable_key());
    let minter = IdentityConfig {
        flame_predicate: nobody.clone(),
        access_predicate: nobody,
        validator_pubkey: ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).verifying_key(),
    }
    .identity(&key);
    let script = minter.witness_script();

    let watch_only = RpcWatchOnlyWallet::open(&ctx.bitcoin_config(), script).await?;
    ctx.pay(Address::p2wsh(script, Network::Regtest), funding)
        .await?;
    Ok(MinterWallet::new(watch_only, key, minter)?)
}

#[tokio::test(flavor = "multi_thread")]
async fn rpc_source_and_wallet_work_with_regtest_blocks_and_transactions()
-> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;

    let first = ctx.source.block_at(1).await?.context("block 1")?;
    let (block, prevouts) = ctx.block(first).await?;
    assert_eq!(block.block_hash(), first.hash);
    assert_eq!(block.txdata.len(), 1);
    // The input of a coinbase spends nothing.
    assert_eq!(prevouts.as_slice(), [vec![None]]);

    let previous_tip = ctx
        .source
        .wait_for_new_block(bitcoin::BlockHash::all_zeros())
        .await?;
    assert_eq!(previous_tip.height, 101);
    // The best chain ends here.
    assert_eq!(ctx.source.block_at(102).await?, None);

    let source = &ctx.source;
    let waiter = source.wait_for_new_block(previous_tip.hash);
    let (next_tip, next_hash) = tokio::join!(waiter, async { ctx.generate_next_block() });
    let next_hash = next_hash?.hash;
    assert_eq!(
        next_tip?,
        BlockId {
            height: 102,
            hash: next_hash
        }
    );

    let recipient = ctx.node.client.new_address()?;
    let amount = Amount::from_btc(1.0).expect("valid BTC amount");
    let payment = TxOut {
        value: amount,
        script_pubkey: recipient.script_pubkey(),
    };
    let expected_txid = ctx.wallet.send(payment).await?;

    let confirmation_block = ctx.generate_next_block()?;
    let (block, prevouts) = ctx.block(confirmation_block).await?;
    let (transaction, prevouts) = block
        .txdata
        .iter()
        .zip(prevouts.as_slice())
        .find(|(transaction, _)| transaction.compute_txid() == expected_txid)
        .context("published transaction was not included in the next block")?;
    assert_eq!(prevouts.len(), transaction.input.len());
    assert!(prevouts.iter().all(Option::is_some));

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wallet_of_bitcoin_core_sends_a_burn() -> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let nobody = Predicate::opaque(Predicate::unspendable_key());
    let burn = BurnData::new(MinterP2wsh::new([0x42; 32]), nobody);
    let sent_amount = Amount::from_sat(25_000);

    let txid = ctx.wallet.send(burn.to_output(sent_amount)).await?;
    let confirmation_block = ctx.generate_next_block()?;
    let (block, prevouts) = ctx.block(confirmation_block).await?;
    let burns = BtcBlockRecords::from_block(confirmation_block, &block, &prevouts)?.burns;

    assert_eq!(burns.len(), 1);
    assert_eq!(burns[0].txid(), txid);
    assert_eq!(burns[0].amount(), sent_amount);
    assert_eq!(burns[0].data(), &burn);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endorsement_spends_two_outputs_when_one_does_not_pay_the_fee()
-> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    // At the fee rate of this node one output of this size does not pay for
    // an endorsement.
    let small = Amount::from_sat(1_400);
    let wallet = minter_wallet(&ctx, small).await?;
    let endorsement = MintingEndorsementData::V1 {
        flame_block_height: 1,
        flame_block_hash: BlockHash::from([0x42; 32]),
    };
    let unpaid = wallet.send(endorsement.to_output()).await;
    assert!(matches!(unpaid, Err(WalletError::Refused(_))));

    // With a second output the same endorsement goes out and spends both.
    let minter = wallet.minter().witness_script().to_p2wsh();
    let address = Address::from_script(&minter, Network::Regtest)?;
    ctx.pay(address, small).await?;
    let txid = wallet.send(endorsement.to_output()).await?;

    let confirmation_block = ctx.generate_next_block()?;
    let (block, prevouts) = ctx.block(confirmation_block).await?;
    let (_, spent) = block
        .txdata
        .iter()
        .zip(prevouts.as_slice())
        .find(|(transaction, _)| transaction.compute_txid() == txid)
        .context("the endorsement was not included in the next block")?;
    let paid = TxOut {
        value: small,
        script_pubkey: minter,
    };
    assert_eq!(spent, &[Some(paid.clone()), Some(paid)]);

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wallet_pays_again_before_a_block_confirms_its_change() -> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    // The wallet has one coin: the reward of block 1.
    let recipient = ctx.node.client.new_address()?.script_pubkey();
    let pay = |sat| TxOut {
        value: Amount::from_sat(sat),
        script_pubkey: recipient.clone(),
    };

    let first = ctx.wallet.send(pay(10_000)).await?;
    // The coin is spent, and its change waits for a block: Bitcoin Core pays
    // the second transaction with that change.
    let second = ctx.wallet.send(pay(20_000)).await?;

    let (block, _) = ctx.block(ctx.generate_next_block()?).await?;
    let mined = |txid| block.txdata.iter().find(|tx| tx.compute_txid() == txid);
    assert!(mined(first).is_some());
    let second = mined(second).context("the second payment is in the block")?;
    assert!(
        second
            .input
            .iter()
            .all(|input| input.previous_output.txid == first)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wallet_is_found_by_a_name_that_an_url_does_not_take_as_it_is()
-> bitcoind::anyhow::Result<()> {
    let ctx = setup()?;
    let name = "coins #1";
    ctx.node.client.create_wallet(name)?;
    let output = TxOut {
        value: Amount::from_sat(10_000),
        script_pubkey: ctx.node.client.new_address()?.script_pubkey(),
    };

    let sent = RpcWallet::new(&ctx.bitcoin_config(), name)?
        .send(output)
        .await;

    // The wallet is new and has no coins. Had it not been found, the refusal
    // would say that there is no such wallet.
    assert!(
        matches!(&sent, Err(WalletError::Refused(why)) if why.contains("Insufficient funds")),
        "{sent:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_node_that_does_not_answer_is_told_apart_from_a_refusal() -> bitcoind::anyhow::Result<()>
{
    // Nothing listens on this port.
    let nowhere = BitcoinConfig {
        node_rpc_url: "http://127.0.0.1:1".to_owned(),
        auth: BitcoinRpcAuth::None,
    };
    let wallet = RpcWallet::new(&nowhere, "coins")?;
    let output = TxOut {
        value: Amount::from_sat(1_000),
        script_pubkey: ScriptBuf::new(),
    };

    let sent = wallet.send(output).await;

    assert!(matches!(sent, Err(WalletError::Unreachable(_))));
    Ok(())
}
