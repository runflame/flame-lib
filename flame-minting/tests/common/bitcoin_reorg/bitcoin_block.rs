use super::*;

pub fn bitcoin_block(
    height: u64,
    branch: u8,
    minter: u8,
    endorsed: &[Arc<Block>],
) -> BtcBlockRecords {
    BtcBlockRecords {
        id: btc_tip(height, branch),
        burns: vec![],
        endorsements: endorsed
            .iter()
            .map(|block| endorsement(minter, core_tip(block)))
            .collect(),
    }
}

fn core_tip(block: &Block) -> CoreBlockTip {
    CoreBlockTip {
        hash: block.header.id(),
        height: block.header.core_block.as_ref().unwrap().height,
    }
}

pub(super) fn btc_tip(height: u64, branch: u8) -> BlockId {
    let mut bytes = [branch; 32];
    bytes[..8].copy_from_slice(&height.to_le_bytes());
    BlockId {
        hash: BitcoinBlockHash::from_byte_array(bytes),
        height,
    }
}

fn witness_script(minter: u8) -> ScriptBuf {
    minter_witness_script::build_with_authorization(
        &Predicate::opaque(Predicate::unspendable_key()),
        &ed25519_dalek::SigningKey::from_bytes(&[minter; 32]).verifying_key(),
        Script::from_bytes(&[minter]),
    )
}

pub(super) fn acquisition(minter: u8, amount: u64) -> Burn {
    let point = Predicate::unspendable_key().decompress().unwrap();
    let access =
        Predicate::opaque((point * flamevm::Scalar::from(u64::from(minter)).to_dalek()).compress());
    let data = BurnData::new(
        witness_script(minter).wscript_hash().to_byte_array().into(),
        access,
    );
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![],
        output: vec![TxOut {
            value: Amount::from_sat(amount),
            script_pubkey: data.to_script(),
        }],
    };
    Burn::from_tx(&tx).pop().unwrap()
}

fn endorsement(minter: u8, tip: CoreBlockTip) -> AuthenticatedMintingEndorsement {
    let script = witness_script(minter);
    let mut payload = b"FLMB".to_vec();
    payload.push(1);
    payload.extend_from_slice(&tip.height.as_u32().to_le_bytes());
    payload.extend_from_slice(tip.hash.as_bytes());
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            witness: Witness::from_slice(&[script.as_bytes()]),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::new_op_return(
                <&bitcoin::script::PushBytes>::try_from(payload.as_slice()).unwrap(),
            ),
        }],
    };
    MintingEndorsementValidator::validate(
        UncheckedMintingEndorsement::from_tx(&tx).pop().unwrap(),
        &TxOut {
            value: Amount::from_sat(1),
            script_pubkey: script.to_p2wsh(),
        },
    )
    .unwrap()
}
