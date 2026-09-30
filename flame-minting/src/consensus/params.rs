use std::num::NonZeroU16;

/// Protocol parameters
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MintingProtocolParams {
    /// Bitcoin blocks before an acquisition starts contributing power.
    pub acquisition_maturity: u32,
    /// Active Bitcoin blocks when the acquisition omits its duration.
    pub default_acquisition_duration: NonZeroU16,
    /// Minimum permitted acquisition duration.
    pub min_acquisition_duration: NonZeroU16,
    /// Votes delayed beyond this many Bitcoin blocks have zero effective power.
    pub max_vote_delay: u32,
}
