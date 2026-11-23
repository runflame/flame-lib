# Bitcoin integration

Flame reads two kinds of records from Bitcoin. A burn gives a minter its
power. An endorsement is the vote of a minter for a core block.

This crate has three parts:

- `watcher` follows the best chain of a Bitcoin node;
- `protocol` reads and builds burns and endorsements;
- `wallet` pays for them and sends them.

## The cycle of a minter

### 1. Follow Bitcoin

The watcher reports every change of the best chain. The blocks an event names
are in the storage by then, with the outputs their inputs spend.

```no_run
use btc_integration::watcher::impls::{RecentBlocks, RpcSource};
use btc_integration::watcher::{BlockId, BtcWatcher};
use btc_integration::{BitcoinConfig, BitcoinRecords};

# async fn follow(bitcoin: BitcoinConfig, genesis_id: BlockId)
# -> Result<(), Box<dyn std::error::Error>> {
let source = RpcSource::new(&bitcoin)?;
// The storage keeps the latest 30 blocks, the rest stays in Bitcoin Core.
let mut watcher = BtcWatcher::new(source, RecentBlocks::new(30), genesis_id);
loop {
    // Waits until the best chain changes. A reorg also names the blocks
    // that left it: `event.removed()`.
    let event = watcher.next().await?;
    for id in event.blocks() {
        match BitcoinRecords::from_storage(watcher.storage(), *id).await {
            // The consensus takes them from here.
            Ok(records) => println!(
                "{id}: {} burns, {} endorsements",
                records.burns.len(),
                records.endorsements.len(),
            ),
            // The source did not give an output that an endorsement spends.
            Err(error) => eprintln!("{id}: {error}"),
        }
    }
}
# }
```

### 2. Pay for the minter

A minter needs two payments, and any wallet can make them:

- an output of the minter script: the endorsements take their fees from it;
- a burn that names the minter.

```no_run
use bitcoin::{Amount, TxOut};
use btc_integration::identity::impls::LocalKey;
use btc_integration::wallet::impls::{RpcWallet, RpcWatchOnlyWallet};
use btc_integration::{BitcoinConfig, BtcWallet, BurnData, IdentityConfig, MinterWallet};

# async fn pay(bitcoin: BitcoinConfig, config: IdentityConfig, key: LocalKey)
# -> Result<(), Box<dyn std::error::Error>> {
let minter = config.identity(&key);

// Bitcoin Core watches the minter script from here on, so this goes before
// the payment to it.
let watch_only = RpcWatchOnlyWallet::open(&bitcoin, minter.witness_script()).await?;
let wallet = MinterWallet::new(watch_only, key, minter)?;
let minter = wallet.minter();

// An ordinary wallet of Bitcoin Core, with coins.
let payer = RpcWallet::new(&bitcoin, "payer")?;

let funding = TxOut {
    value: Amount::from_sat(100_000),
    script_pubkey: minter.witness_script().to_p2wsh(),
};
payer.send(funding).await?;

let burn = BurnData::new(minter.p2wsh(), config.access_predicate.clone()).with_duration(1_000);
payer.send(burn.to_output(Amount::from_sat(50_000))).await?;
# Ok(())
# }
```

### 3. Endorse core blocks

The wallet of the minter sends one endorsement for a core block. Bitcoin Core
finds the outputs of the minter script and builds the transaction, the key of
the minter signs it.

```no_run
use btc_integration::identity::impls::LocalKey;
use btc_integration::wallet::impls::RpcWatchOnlyWallet;
use btc_integration::{BtcWallet, MinterWallet, MintingEndorsementData};

# async fn endorse(wallet: MinterWallet<RpcWatchOnlyWallet, LocalKey>, height: u32,
# hash: flamechain::BlockHash) -> Result<(), Box<dyn std::error::Error>> {
let endorsement = MintingEndorsementData::V1 {
    flame_block_height: height,
    flame_block_hash: hash,
};
wallet.send(endorsement.to_output()).await?;
# Ok(())
# }
```

An endorsement spends a confirmed output of the minter script and returns the
change there. So one endorsement goes out per Bitcoin block: the next one
waits until the change is in a block, and `send` fails until then.

### What runs out

Both payments of step 2 have to be repeated, and nothing in this crate tells
when:

- **The burn.** It gives power for `duration` Bitcoin blocks, or for the
  default of the protocol when it names none. The amount is spread evenly over
  them, and the burns of one minter add up. An endorsement sent after the last
  burn has ended is still a valid transaction and still pays its fee, but it
  weighs nothing. To leave no gap, the next burn has to be in a Bitcoin block
  before the previous one ends.
- **The outputs of the minter script.** Every endorsement takes its fee from
  them. When they are spent, `send` returns an error until somebody pays to the
  script again.
