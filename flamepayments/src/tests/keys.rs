//! Accounts: the public half of every key, address discovery, and one
//! pinned vector so a change to the derivation path cannot pass silently.

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_TABLE;
use flamekd::{util, Network, HARDENED};

use crate::keys::{Account, KeyError};

const SEED: [u8; 64] = [0x33; 64];

/// A few `(branch, n)` pairs on both branches.
const PAIRS: [(u32, u32); 5] = [
    (util::RECEIVING, 0),
    (util::RECEIVING, 1),
    (util::RECEIVING, 9),
    (util::CHANGE, 0),
    (util::CHANGE, 21),
];

fn account() -> Account {
    Account::from_seed(&SEED, Network::Testnet).expect("account from seed")
}

#[test]
fn a_predicate_is_the_public_half_of_its_spending_key() {
    let account = account();
    for (branch, n) in PAIRS {
        let secret = account.spending_key_at(branch, n).expect("spending key");
        assert_eq!(
            account
                .predicate_at(branch, n)
                .expect("predicate")
                .to_point(),
            (RISTRETTO_BASEPOINT_TABLE * &secret).compress(),
            "predicate at {branch}/{n}"
        );
    }
}

#[test]
fn an_address_carries_the_same_keys_as_the_account_derives() {
    let account = account();
    for (branch, n) in PAIRS {
        let address = account.address_at(branch, n).expect("address");
        let spending = account.spending_key_at(branch, n).expect("spending key");
        let viewing = account.viewing_key_at(branch, n).expect("viewing key");
        assert_eq!(
            address.spending_key().compress(),
            (RISTRETTO_BASEPOINT_TABLE * &spending).compress(),
            "S at {branch}/{n}"
        );
        assert_eq!(
            address.viewing_key().compress(),
            (RISTRETTO_BASEPOINT_TABLE * &viewing).compress(),
            "V at {branch}/{n}"
        );
    }
}

#[test]
fn the_branch_receiving_key_derives_the_same_addresses() {
    let account = account();
    assert_eq!(
        account
            .recv_key()
            .derive_child(util::RECEIVING)
            .expect("branch key"),
        account.recv_key_for(util::RECEIVING).expect("branch key"),
        "recv_key_for is the account recv key's branch child"
    );

    let branch = account.recv_key_for(util::RECEIVING).expect("branch key");
    for n in [0, 1, 9] {
        assert_eq!(
            branch.derive_child(n).expect("child").to_address(),
            account.address_at(util::RECEIVING, n).expect("address"),
            "address at receiving/{n}"
        );
    }
}

#[test]
fn next_address_hands_out_consecutive_receiving_addresses() {
    let mut account = account();
    for expected in 0..3 {
        let (index, address) = account.next_address().expect("next address");
        assert_eq!(index, expected);
        assert_eq!(
            address,
            account
                .address_at(util::RECEIVING, expected)
                .expect("address")
        );
    }
    assert_eq!(account.next_index(), 3);
}

#[test]
fn owns_finds_every_address_below_the_gap_and_nothing_above() {
    let mut account = account();
    for _ in 0..3 {
        account.next_address().expect("next address");
    }

    let gap = 4;
    let limit = account.next_index() + gap;
    for branch in [util::RECEIVING, util::CHANGE] {
        for n in 0..limit {
            let point = account
                .address_at(branch, n)
                .expect("address")
                .spending_key()
                .compress();
            assert_eq!(
                account.owns(&point, gap),
                Some((branch, n)),
                "owns {branch}/{n} below the gap"
            );
        }
        let above = account
            .address_at(branch, limit)
            .expect("address")
            .spending_key()
            .compress();
        assert_eq!(
            account.owns(&above, gap),
            None,
            "{branch}/{limit} is past the gap"
        );
    }
}

#[test]
fn a_hardened_branch_or_index_is_refused() {
    let account = account();
    assert_eq!(
        account.address_at(HARDENED, 0),
        Err(KeyError::HardenedIndex(HARDENED))
    );
    assert_eq!(
        account.address_at(util::RECEIVING, HARDENED | 4),
        Err(KeyError::HardenedIndex(HARDENED | 4))
    );
    assert_eq!(
        account.spending_key_at(HARDENED, 0),
        Err(KeyError::HardenedIndex(HARDENED))
    );
    assert_eq!(
        account.recv_key_for(HARDENED | 1),
        Err(KeyError::HardenedIndex(HARDENED | 1))
    );
}

/// Generated once from this seed and pasted in. If the account path, the
/// branch order or flamekd's derivation changes, this is what notices.
#[test]
fn pinned_testnet_address() {
    let account = Account::from_seed(&[0x11; 64], Network::Testnet).expect("account from seed");
    let address = account
        .address_at(util::RECEIVING, 0)
        .expect("first receiving address");

    assert_eq!(
        address.to_bech32(Network::Testnet),
        "tf15qsfa4dqsckg4talyc7h5xu26psmyaklrmn3d2ne9dzmkm6aqfvqchl5ngnhyljv2xu0d0a6uf57ju840358vej6pxt7vxff3mywgqgcnpfax"
    );
    assert_eq!(
        hex::encode(address.to_bytes()),
        "a0209ed5a0862c8aafbf263d7a1b8ad061b276df1ee716aa792b45bb6f5d02580c5ff49a27727e4c51b8f6bfbae269e970f57c6876665a0997e619298ec8e401"
    );
}
