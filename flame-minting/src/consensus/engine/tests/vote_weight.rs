use super::*;
use crate::consensus::engine::vote_weight::{VoteWeightError, weight_vote};

pub(super) fn params() -> MintingProtocolParams {
    MintingProtocolParams {
        acquisition_maturity: 2,
        default_acquisition_duration: 5.try_into().unwrap(),
        min_acquisition_duration: 1.try_into().unwrap(),
        max_vote_delay: 10,
    }
}

fn included_vote(inclusion_height: u64) -> IncludedVote {
    IncludedVote {
        btc_block: btc_tip(inclusion_height),
        vote: vote(0x51, 42, 1),
    }
}

pub(super) fn included_acquisition(
    minter: u8,
    height: u64,
    amount: u64,
    duration: Option<u16>,
) -> IncludedAcquisition {
    IncludedAcquisition {
        btc_block: btc_tip(height),
        acquisition: acquisition_with_amount(
            vote(minter, 42, 1).auth().minter().p2wsh(),
            duration,
            amount,
        ),
    }
}

#[test]
fn sums_acquisition_power_before_applying_delay_and_preserves_the_vote() {
    let vote = included_vote(101);
    let acquisitions = [
        included_acquisition(0x51, 98, 19, Some(4)),
        included_acquisition(0x51, 98, 7, Some(2)),
        included_acquisition(0x51, 98, 21, None),
    ];
    let weighted = weight_vote(vote.clone(), 100, &acquisitions, &params()).unwrap();

    // floor(19/4) + floor(7/2) + floor(21/5) = 11; floor(11/2) = 5.
    assert_eq!(weighted.effective_minting_power, 5);
    assert_eq!(weighted.original, vote);
}

#[test]
fn applies_delay_at_and_beyond_the_configured_limit() {
    let acquisitions = [included_acquisition(0x51, 98, 16384, Some(4))];
    for (delay, expected) in [(0, 4096), (1, 2048), (2, 1024), (10, 4), (11, 0)] {
        // Activity is evaluated at 100, even after the acquisition expires at 104.
        let weighted =
            weight_vote(included_vote(100 + delay), 100, &acquisitions, &params()).unwrap();
        assert_eq!(weighted.effective_minting_power, expected, "delay={delay}");
    }

    let mut params = params();
    params.max_vote_delay = 1;
    let weighted = weight_vote(included_vote(102), 100, &acquisitions, &params).unwrap();
    assert_eq!(weighted.effective_minting_power, 0);
}

#[test]
fn counts_only_the_minters_acquisitions_active_at_the_target() {
    let acquisitions = [
        included_acquisition(0x51, 98, 15, Some(3)), // Active: 100..103.
        included_acquisition(0x51, 90, 1000, Some(8)), // Expired at 100.
        included_acquisition(0x51, 99, 1000, Some(5)), // Matures at 101.
        included_acquisition(0x51, 101, 1000, Some(5)), // Acquired after target.
        included_acquisition(0x52, 98, 1000, Some(5)), // Another minter.
    ];
    let weighted = weight_vote(included_vote(100), 100, &acquisitions, &params()).unwrap();
    assert_eq!(weighted.effective_minting_power, 5);
}

#[test]
fn returns_zero_when_no_acquisition_contributes_power() {
    for acquisitions in [vec![], vec![included_acquisition(0x51, 98, 1, Some(2))]] {
        let weighted = weight_vote(included_vote(100), 100, &acquisitions, &params()).unwrap();
        assert_eq!(weighted.effective_minting_power, 0);
    }
}

#[test]
fn rejects_a_vote_included_before_its_target() {
    assert_eq!(
        weight_vote(included_vote(99), 100, &[], &params()),
        Err(VoteWeightError::VoteBeforeTarget {
            inclusion_height: 99,
            target_height: 100,
        })
    );
}

#[test]
fn handles_delays_larger_than_the_integer_width() {
    let acquisitions = [included_acquisition(0x51, 98, u64::MAX, Some(1))];
    let mut params = params();
    params.max_vote_delay = u32::MAX;
    for (inclusion_height, expected) in [(163, 1), (164, 0), (u64::MAX, 0)] {
        let weighted =
            weight_vote(included_vote(inclusion_height), 100, &acquisitions, &params).unwrap();
        assert_eq!(weighted.effective_minting_power, expected);
    }
}

#[test]
fn reports_minting_power_overflow() {
    let acquisitions = [
        included_acquisition(0x51, 98, u64::MAX, Some(1)),
        included_acquisition(0x51, 98, 1, Some(1)),
    ];
    assert_eq!(
        weight_vote(included_vote(100), 100, &acquisitions, &params()),
        Err(VoteWeightError::MintingPowerOverflow)
    );
}
