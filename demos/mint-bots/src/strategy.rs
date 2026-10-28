//! What the bots decide, from the playground's schedules alone. Pure
//! functions: the runner feeds them a snapshot and acts on the answer.
//!
//! The playground's rules, as the README states them: a block pays 50 FLM
//! to its eligible burns in proportion; Independent and Service
//! contributions pay 1,000 sat of fees per block on top of their burn; a
//! delegation burns its whole budget and is eligible only while its service
//! has an active Service contribution.

use crate::playground::{Schedule, Service};

pub const FEE_PER_BLOCK: i64 = 1_000;
pub const MIN_DURATION: i64 = 144;
const BLOCK_REWARD_FLM: f64 = 50.0;

/// The smallest contribution that pays its fees and burns a sat a block.
pub const MIN_BUDGET: i64 = MIN_DURATION * (FEE_PER_BLOCK + 1);

/// The clock and farmer bots start this many blocks before the gap they
/// see: the playground counts a delay from its height when it accepts the
/// submission, a block or two after the snapshot the delay came from.
const SLACK: i64 = 3;

/// The longest contribution the farmer bot makes at once: a day of blocks.
pub const MAX_COVER: i64 = 86_400;

/// Durations the minimax bot weighs, up to the UI's 31 days.
const DURATIONS: [i64; 6] = [144, 288, 576, 1_152, 2_304, 4_464];

/// Its start candidates are this many blocks apart.
const START_STEP: i64 = 30;

/// How far behind its pace, in chunks, the minimax bot may fall before it
/// takes the best window on offer whatever its price.
const BEHIND: i64 = 4;

fn delegating(schedule: &Schedule) -> bool {
    schedule.mode == "delegating"
}

fn end(schedule: &Schedule) -> i64 {
    schedule.start_block.saturating_add(schedule.duration)
}

fn active(schedule: &Schedule, block: i64) -> bool {
    schedule.start_block <= block && block < end(schedule)
}

/// Sats burned per block, without the one extra sat of the first
/// remainder blocks.
pub fn burn(schedule: &Schedule) -> i64 {
    let fee = if delegating(schedule) {
        0
    } else {
        FEE_PER_BLOCK
    };
    let duration = schedule.duration.max(1);
    (schedule.budget_sats - duration.saturating_mul(fee)) / duration
}

/// The first block at or after `from` with no Independent or Service
/// contribution. A delegation alone never fills a block: it is eligible
/// only while its service burns too.
pub fn first_idle_block(schedules: &[Schedule], from: i64) -> i64 {
    let mut windows: Vec<_> = schedules
        .iter()
        .filter(|schedule| !delegating(schedule) && end(schedule) > from)
        .map(|schedule| (schedule.start_block, end(schedule)))
        .collect();
    windows.sort_unstable();
    let mut block = from;
    for (start, end) in windows {
        if start > block {
            break;
        }
        block = block.max(end);
    }
    block
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub budget_sats: i64,
    pub duration: i64,
    pub delay_blocks: i64,
    /// Why, as the journal keeps it.
    pub reason: serde_json::Value,
}

/// A minimal contribution if the next idle block is within `lookahead`
/// blocks of `height`, the playground's last committed block.
pub fn clock_in(
    schedules: &[Schedule],
    height: i64,
    burn_per_block: i64,
    duration: i64,
    lookahead: i64,
) -> Option<Plan> {
    let next = height + 1;
    let idle = first_idle_block(schedules, next);
    if idle - next > lookahead {
        return None;
    }
    Some(Plan {
        budget_sats: (burn_per_block + FEE_PER_BLOCK) * duration,
        duration,
        delay_blocks: (idle - next - SLACK).max(0),
        reason: serde_json::json!({ "idle_from_block": idle }),
    })
}

/// The first block at or after `from` where `me` has no Service
/// contribution: where its service, and every delegation to it, stops
/// earning.
pub fn served_until(schedules: &[Schedule], me: i64, from: i64) -> i64 {
    let mine: Vec<Schedule> = schedules
        .iter()
        .filter(|schedule| schedule.user_id == me && schedule.mode == "service")
        .cloned()
        .collect();
    first_idle_block(&mine, from)
}

/// The end of the last delegation to `me`, or `from` if none outlasts it.
pub fn delegated_until(schedules: &[Schedule], me: i64, from: i64) -> i64 {
    schedules
        .iter()
        .filter(|schedule| delegating(schedule) && schedule.service_user_id == Some(me))
        .map(end)
        .fold(from, i64::max)
}

/// Players the farmer bot's offer is for at `block`: minters other than the
/// bots `ours` minting Independent there, burning less in all than
/// `break_even`, so they would earn more delegating to it. `None` is no
/// limit: a free service suits everyone.
pub fn prospects(schedules: &[Schedule], ours: &[i64], block: i64, break_even: Option<i64>) -> i64 {
    let mut burns: std::collections::HashMap<i64, i64> = Default::default();
    for schedule in schedules {
        if schedule.mode == "independent"
            && active(schedule, block)
            && !ours.contains(&schedule.user_id)
        {
            *burns.entry(schedule.user_id).or_default() += burn(schedule);
        }
    }
    burns
        .values()
        .filter(|&&burn| break_even.is_none_or(|limit| burn < limit))
        .count() as i64
}

/// A Service contribution for the farmer bot `me`. While `open`, it keeps
/// serving: once its service ends within `lookahead` blocks of `height`, it
/// adds the next contribution where the service ends. Open or not, a
/// delegation to it that outlasts its service is covered at once, to its
/// end, so a delegator never burns ineligibly while the bot runs, nor after
/// it closes or is paused.
pub fn farm(
    schedules: &[Schedule],
    me: i64,
    height: i64,
    burn_per_block: i64,
    duration: i64,
    lookahead: i64,
    open: bool,
) -> Option<Plan> {
    let next = height + 1;
    let served = served_until(schedules, me, next);
    let promised = delegated_until(schedules, me, next);
    let start = if promised > served && !open {
        // Closed: only from where a delegation to it first needs it.
        schedules
            .iter()
            .filter(|schedule| {
                delegating(schedule)
                    && schedule.service_user_id == Some(me)
                    && end(schedule) > served
            })
            .map(|schedule| schedule.start_block.max(served))
            .min()
            .unwrap_or(served)
    } else if promised > served || (open && served - next <= lookahead) {
        served
    } else {
        return None;
    };
    let delay_blocks = (start - next - SLACK).max(0);
    // From where it starts, slack included: ending a few blocks short would
    // take a whole new contribution to cover them.
    let duration = (promised - next - delay_blocks).clamp(duration, MAX_COVER.max(duration));
    Some(Plan {
        budget_sats: (burn_per_block + FEE_PER_BLOCK) * duration,
        duration,
        delay_blocks,
        reason: serde_json::json!({ "open": open, "served_until_block": served, "delegated_until_block": promised }),
    })
}

/// The cheapest rival: the lowest commission among other active services.
pub fn rival<'a>(services: &'a [Service], me: Option<i64>) -> Option<&'a Service> {
    services
        .iter()
        .filter(|service| service.active && Some(service.user_id) != me)
        .min_by_key(|service| service.commission_bps)
}

/// The largest burn, in sat a block, for which delegating at `commission_bps`
/// earns more FLM per sat than minting independently, which pays the
/// 1,000-sat fee a block on top of the same burn: `1,000 × (1 − c) / c`.
pub fn break_even_burn(commission_bps: i64) -> Option<i64> {
    (commission_bps > 0).then(|| FEE_PER_BLOCK * (10_000 - commission_bps) / commission_bps)
}

/// Eligible burn per block, constant over `[start, end)`: other minters'
/// and the bot's own.
#[derive(Debug, PartialEq)]
struct Segment {
    start: i64,
    end: i64,
    others: f64,
    own: f64,
}

/// What competes for the rewards of `[from, to)` with a new contribution of
/// the bot `me`. Delegations to `me` count in full: the new contribution
/// makes them eligible where it runs, which is the only place it is
/// weighed. Delegations to other services count where those burn.
fn profile(schedules: &[Schedule], me: i64, from: i64, to: i64) -> Vec<Segment> {
    let mut services: std::collections::HashMap<i64, Vec<(i64, i64)>> = Default::default();
    for schedule in schedules
        .iter()
        .filter(|schedule| schedule.mode == "service")
    {
        services
            .entry(schedule.user_id)
            .or_default()
            .push((schedule.start_block, end(schedule)));
    }
    for windows in services.values_mut() {
        windows.sort_unstable();
        let mut merged: Vec<(i64, i64)> = Vec::with_capacity(windows.len());
        for &(start, end) in windows.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        *windows = merged;
    }
    // (block, change of others' burn, change of own burn)
    let mut changes: Vec<(i64, f64, f64)> = Vec::new();
    let mut add = |start: i64, end: i64, burn: f64, own: bool| {
        let (start, end) = (start.max(from), end.min(to));
        if start < end {
            let (others, own) = if own { (0.0, burn) } else { (burn, 0.0) };
            changes.push((start, others, own));
            changes.push((end, -others, -own));
        }
    };
    for schedule in schedules {
        let rate = burn(schedule) as f64;
        match schedule.service_user_id {
            _ if schedule.user_id == me => add(schedule.start_block, end(schedule), rate, true),
            Some(service) if delegating(schedule) && service != me => {
                for &(start, stop) in services.get(&service).into_iter().flatten() {
                    add(
                        schedule.start_block.max(start),
                        end(schedule).min(stop),
                        rate,
                        false,
                    );
                }
            }
            _ => add(schedule.start_block, end(schedule), rate, false),
        }
    }
    changes.sort_unstable_by_key(|change| change.0);
    let mut segments = Vec::new();
    let (mut block, mut others, mut own) = (from, 0.0, 0.0);
    for (at, d_others, d_own) in changes {
        if at > block {
            segments.push(Segment {
                start: block,
                end: at,
                others,
                own,
            });
            block = at;
        }
        others += d_others;
        own += d_own;
    }
    if block < to {
        segments.push(Segment {
            start: block,
            end: to,
            others,
            own,
        });
    }
    segments
}

/// FLM a burn of `burn` sats a block over `[start, end)` adds to what the
/// bot already earns there.
fn gain(segments: &[Segment], start: i64, end: i64, burn: f64) -> f64 {
    segments
        .iter()
        .filter(|segment| segment.start < end && start < segment.end)
        .map(|segment| {
            let blocks = (segment.end.min(end) - segment.start.max(start)) as f64;
            let share = |own: f64| {
                if own + segment.others > 0.0 {
                    own / (own + segment.others)
                } else {
                    0.0
                }
            };
            blocks * BLOCK_REWARD_FLM * (share(segment.own + burn) - share(segment.own))
        })
        .sum()
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Window {
    pub delay_blocks: i64,
    pub duration: i64,
    pub burn_per_block: i64,
    pub expected_flm: f64,
    /// Sats, fees included, per expected FLM.
    pub price: f64,
}

/// The cheapest way to spend `budget` sats as a Service contribution
/// starting within `lookahead` blocks, given what is scheduled now. Later
/// arrivals are unknown and not guessed at.
pub fn cheapest_window(
    schedules: &[Schedule],
    me: i64,
    height: i64,
    budget: i64,
    lookahead: i64,
) -> Option<Window> {
    let from = height + 1;
    let segments = profile(schedules, me, from, from + lookahead + DURATIONS[5]);
    let mut best: Option<Window> = None;
    for duration in DURATIONS {
        let burn_per_block = budget / duration - FEE_PER_BLOCK;
        if burn_per_block < 1 {
            continue;
        }
        for delay in (0..=lookahead).step_by(START_STEP as usize) {
            let start = from + delay;
            let expected_flm = gain(&segments, start, start + duration, burn_per_block as f64);
            if expected_flm <= 0.0 {
                continue;
            }
            let price = budget as f64 / expected_flm;
            if best.as_ref().is_none_or(|best| price < best.price) {
                best = Some(Window {
                    delay_blocks: delay,
                    duration,
                    burn_per_block,
                    expected_flm,
                    price,
                });
            }
        }
    }
    best
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Pace {
    /// What the bot may have committed by now: the budget, spread evenly
    /// over its window.
    pub allowance: i64,
    /// One contribution: an hour of the even pace, at least two minimal
    /// ones, at most what is left.
    pub chunk: i64,
}

/// Times in unix seconds.
pub fn pace(budget: i64, committed: i64, starts_at: i64, ends_at: i64, now: i64) -> Pace {
    let length = (ends_at - starts_at).max(1) as i128;
    let elapsed = (now - starts_at).clamp(0, length as i64) as i128;
    let allowance = (budget as i128 * elapsed / length) as i64;
    let hourly = (budget as i128 * 3_600 / length) as i64;
    let chunk = hourly.max(2 * MIN_BUDGET).min((budget - committed).max(0));
    Pace { allowance, chunk }
}

/// Whether the minimax bot spends a chunk now: when its pace allows it and
/// the price is at most the recent going rate, or whatever the price once
/// it has fallen far enough behind.
pub fn should_spend(pace: Pace, committed: i64, price: f64, reference: f64) -> bool {
    let available = pace.allowance - committed;
    pace.chunk >= MIN_BUDGET
        && available >= pace.chunk
        && (price <= reference || available >= BEHIND * pace.chunk)
}

/// What a bot's telemetry reports about a block.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
pub struct Live {
    /// The bot's own burn.
    pub own_burn: i64,
    /// Burn of delegations to the bot, eligible or not.
    pub delegated_burn: i64,
    /// Distinct delegators active in the block.
    pub delegators: i64,
    /// Budgets of every delegation to the bot so far, finished, running or
    /// scheduled.
    pub delegated_budget: i64,
    /// Everyone's eligible burn.
    pub network_burn: i64,
}

pub fn live(schedules: &[Schedule], me: Option<i64>, block: i64) -> Live {
    let mut live = Live::default();
    let mut delegators = std::collections::HashSet::new();
    let serving: std::collections::HashSet<i64> = schedules
        .iter()
        .filter(|schedule| schedule.mode == "service" && active(schedule, block))
        .map(|schedule| schedule.user_id)
        .collect();
    for schedule in schedules {
        let to_me = me.is_some() && delegating(schedule) && schedule.service_user_id == me;
        if to_me {
            live.delegated_budget += schedule.budget_sats;
        }
        if !active(schedule, block) {
            continue;
        }
        let rate = burn(schedule);
        if me == Some(schedule.user_id) {
            live.own_burn += rate;
        }
        if to_me {
            live.delegated_burn += rate;
            delegators.insert(schedule.user_id);
        }
        if !delegating(schedule)
            || schedule
                .service_user_id
                .is_some_and(|service| serving.contains(&service))
        {
            live.network_burn += rate;
        }
    }
    live.delegators = delegators.len() as i64;
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(user_id: i64, start_block: i64, duration: i64, burn: i64, mode: &str) -> Schedule {
        let fee = if mode == "delegating" {
            0
        } else {
            FEE_PER_BLOCK
        };
        Schedule {
            user_id,
            budget_sats: (burn + fee) * duration,
            duration,
            start_block,
            mode: mode.into(),
            service_user_id: None,
        }
    }

    fn delegation(
        user_id: i64,
        service: i64,
        start_block: i64,
        duration: i64,
        burn: i64,
    ) -> Schedule {
        Schedule {
            service_user_id: Some(service),
            ..schedule(user_id, start_block, duration, burn, "delegating")
        }
    }

    #[test]
    fn idle_block_skips_chained_and_overlapping_windows_but_not_lone_delegations() {
        let schedules = [
            schedule(1, 11, 144, 5, "independent"),
            schedule(2, 100, 144, 5, "service"),
            schedule(3, 400, 144, 5, "independent"),
            delegation(4, 2, 200, 1_000, 5),
        ];
        assert_eq!(first_idle_block(&schedules, 11), 244);
        assert_eq!(first_idle_block(&schedules, 5), 5);
        assert_eq!(first_idle_block(&schedules, 300), 300);
        assert_eq!(first_idle_block(&[], 7), 7);
    }

    #[test]
    fn clock_waits_until_the_gap_is_near_and_starts_just_before_it() {
        let schedules = [schedule(1, 11, 144, 5, "independent")];
        // Idle from 155: 145 blocks away from 10, beyond a 30-block lookahead.
        assert_eq!(clock_in(&schedules, 10, 1, 144, 30), None);
        let plan = clock_in(&schedules, 130, 1, 144, 30).unwrap();
        assert_eq!(plan.delay_blocks, 155 - 131 - SLACK);
        assert_eq!(plan.budget_sats, 1_001 * 144);
        assert_eq!(plan.duration, 144);
        // Already idle: start now.
        assert_eq!(clock_in(&[], 10, 1, 144, 30).unwrap().delay_blocks, 0);
    }

    #[test]
    fn farmer_extends_its_service_and_covers_every_delegation_to_it() {
        // Nothing yet: serve from the next block.
        let plan = farm(&[], 9, 10, 1, 1_440, 60, true).unwrap();
        assert_eq!((plan.delay_blocks, plan.duration), (0, 1_440));
        assert_eq!(plan.budget_sats, 1_001 * 1_440);

        // Serving until 1,011: nothing to do until that end is near, then
        // chain the next contribution onto it.
        let serving = [schedule(9, 11, 1_000, 1, "service")];
        assert_eq!(farm(&serving, 9, 10, 1, 1_440, 60, true), None);
        let plan = farm(&serving, 9, 960, 1, 1_440, 60, true).unwrap();
        assert_eq!(plan.delay_blocks, 1_011 - 961 - SLACK);
        // Someone else's service or an independent contribution does not count.
        let others = [
            schedule(1, 0, 5_000, 1, "service"),
            schedule(9, 0, 5_000, 1, "independent"),
        ];
        assert_eq!(
            farm(&others, 9, 10, 1, 1_440, 60, true)
                .unwrap()
                .delay_blocks,
            0
        );

        // A delegation to it running past its service: covered at once, to its end.
        let delegated = [serving[0].clone(), delegation(3, 9, 500, 3_000, 5)];
        let plan = farm(&delegated, 9, 10, 1, 1_440, 60, true).unwrap();
        assert_eq!(plan.delay_blocks, 1_011 - 11 - SLACK);
        // It starts a little early, and still ends where the delegation does.
        assert_eq!(11 + plan.delay_blocks + plan.duration, 3_500);
        let after = [
            serving[0].clone(),
            delegated[1].clone(),
            schedule(9, 11 + plan.delay_blocks, plan.duration, 1, "service"),
        ];
        assert_eq!(farm(&after, 9, 10, 1, 1_440, 60, true), None);
        // Delegations to others are not its promise.
        let elsewhere = [serving[0].clone(), delegation(3, 2, 500, 3_000, 5)];
        assert_eq!(farm(&elsewhere, 9, 10, 1, 1_440, 60, true), None);
        // A very long one is covered a day at a time.
        let long = [delegation(3, 9, 0, 1_000_000, 5)];
        assert_eq!(
            farm(&long, 9, 10, 1, 1_440, 60, true).unwrap().duration,
            MAX_COVER
        );
    }

    #[test]
    fn closed_farmer_spends_nothing_but_still_covers_delegations_to_it() {
        let farm_closed =
            |schedules: &[Schedule], height| farm(schedules, 9, height, 1, 1_440, 60, false);
        // No crowd, no service: silent.
        assert_eq!(farm_closed(&[], 10), None);
        let serving = [schedule(9, 11, 1_000, 1, "service")];
        assert_eq!(farm_closed(&serving, 960), None);
        // A delegation from block 2,000 to 2,500: covered just for it, now.
        let delegated = [delegation(3, 9, 2_000, 500, 5)];
        let plan = farm_closed(&delegated, 10).unwrap();
        assert_eq!(plan.delay_blocks, 2_000 - 11 - SLACK);
        assert_eq!(plan.duration, 1_440);
        // Already running into it: from where its service ends.
        let partly = [serving[0].clone(), delegation(3, 9, 500, 3_000, 5)];
        let plan = farm_closed(&partly, 10).unwrap();
        assert_eq!(plan.delay_blocks, 1_011 - 11 - SLACK);
        assert_eq!(11 + plan.delay_blocks + plan.duration, 3_500);
    }

    #[test]
    fn prospects_are_other_small_independent_minters() {
        let schedules = [
            schedule(1, 0, 200, 50_000, "independent"),
            // Two contributions of one player count together.
            schedule(2, 0, 200, 60_000, "independent"),
            schedule(2, 0, 200, 60_000, "independent"),
            schedule(3, 0, 200, 10, "service"),
            delegation(4, 3, 0, 200, 10),
            // Ours, and someone not minting yet.
            schedule(7, 0, 200, 1, "independent"),
            schedule(5, 150, 200, 10, "independent"),
        ];
        assert_eq!(prospects(&schedules, &[7], 100, Some(99_000)), 1);
        assert_eq!(prospects(&schedules, &[7], 100, None), 2);
        assert_eq!(prospects(&schedules, &[], 100, Some(99_000)), 2);
        assert_eq!(prospects(&schedules, &[7], 160, Some(99_000)), 2);
        assert_eq!(prospects(&schedules, &[7], 300, Some(99_000)), 1);
    }

    #[test]
    fn rival_and_break_even() {
        let service = |user_id, commission_bps, active| Service {
            user_id,
            name: String::new(),
            commission_bps,
            active,
        };
        let services = [
            service(9, 50, true),
            service(2, 100, false),
            service(3, 300, true),
            service(4, 200, true),
        ];
        assert_eq!(rival(&services, Some(9)).unwrap().user_id, 4);
        assert_eq!(rival(&services, None).unwrap().user_id, 9);
        assert!(rival(&services[..2], Some(9)).is_none());
        // 1%: delegating pays for anyone burning under 99,000 sat a block.
        assert_eq!(break_even_burn(100), Some(99_000));
        assert_eq!(break_even_burn(10_000), Some(0));
        assert_eq!(break_even_burn(0), None);
    }

    #[test]
    fn profile_separates_own_burn_and_counts_delegations_only_while_their_service_runs() {
        let schedules = [
            schedule(1, 0, 200, 10, "independent"),
            schedule(9, 50, 144, 7, "service"),
            schedule(2, 100, 200, 4, "service"),
            delegation(3, 2, 0, 400, 3),
            delegation(4, 9, 0, 400, 2),
        ];
        let segments = profile(&schedules, 9, 0, 400);
        let at = |block: i64| {
            let segment = segments
                .iter()
                .find(|segment| segment.start <= block && block < segment.end)
                .unwrap();
            (segment.others, segment.own)
        };
        // Delegations to the bot count throughout, its own burn separately.
        assert_eq!(at(10), (12.0, 0.0));
        assert_eq!(at(60), (12.0, 7.0));
        // User 2's delegate only while user 2 serves.
        assert_eq!(at(150), (19.0, 7.0));
        assert_eq!(at(250), (9.0, 0.0));
        assert_eq!(at(350), (2.0, 0.0));
        assert_eq!(segments.first().unwrap().start, 0);
        assert_eq!(segments.last().unwrap().end, 400);
    }

    #[test]
    fn gain_is_the_marginal_share_of_each_block() {
        let segments = [
            Segment {
                start: 0,
                end: 10,
                others: 0.0,
                own: 0.0,
            },
            Segment {
                start: 10,
                end: 20,
                others: 100.0,
                own: 0.0,
            },
            Segment {
                start: 20,
                end: 30,
                others: 100.0,
                own: 100.0,
            },
        ];
        assert_eq!(gain(&segments, 0, 10, 1.0), 500.0);
        assert_eq!(gain(&segments, 10, 20, 100.0), 250.0);
        // From a half to two thirds of each block.
        let third = gain(&segments, 20, 30, 100.0);
        assert!((third - 10.0 * 50.0 / 6.0).abs() < 1e-9);
        assert_eq!(gain(&segments, 5, 15, 100.0), 250.0 + 125.0);
    }

    #[test]
    fn cheapest_window_waits_for_a_competitor_to_finish() {
        // A heavy minter for the next 600 blocks, nothing after.
        let schedules = [schedule(1, 1, 600, 1_000_000, "independent")];
        let window = cheapest_window(&schedules, 9, 0, 1_000_000, 600).unwrap();
        assert!(window.delay_blocks >= 600, "{window:?}");
        // Alone, the whole reward of every block is the bot's.
        assert_eq!(window.expected_flm, 50.0 * window.duration as f64);
        // Too small a budget for any duration.
        assert_eq!(cheapest_window(&[], 9, 0, MIN_BUDGET - 1, 600), None);
    }

    #[test]
    fn pace_spreads_the_budget_and_spending_waits_for_a_fair_price_until_far_behind() {
        let day = 86_400;
        let pace_at = |committed, now| pace(24_000_000, committed, 0, day, now);
        assert_eq!(
            pace_at(0, 0),
            Pace {
                allowance: 0,
                chunk: 1_000_000
            }
        );
        assert_eq!(pace_at(0, day / 2).allowance, 12_000_000);
        assert_eq!(pace_at(0, 2 * day).allowance, 24_000_000);
        assert_eq!(pace_at(23_500_000, day).chunk, 500_000);
        assert_eq!(pace_at(24_000_000, day).chunk, 0);

        let hour = 3_600;
        // An hour in: one chunk available, spent only at a fair price.
        assert!(!should_spend(pace_at(0, hour / 2), 0, 1.0, 2.0));
        assert!(should_spend(pace_at(0, hour), 0, 1.0, 2.0));
        assert!(!should_spend(pace_at(0, hour), 0, 3.0, 2.0));
        // Four hours behind: at any price.
        assert!(should_spend(pace_at(0, 4 * hour), 0, 3.0, 2.0));
        assert!(!should_spend(
            pace_at(3_000_000, 4 * hour),
            3_000_000,
            3.0,
            2.0
        ));
        // Dust below a minimal contribution is never spent.
        assert!(!should_spend(
            pace_at(23_900_000, 2 * day),
            23_900_000,
            1.0,
            2.0
        ));
    }

    #[test]
    fn live_reports_own_delegated_and_eligible_network_burn() {
        let schedules = [
            schedule(9, 0, 200, 7, "service"),
            delegation(3, 9, 0, 400, 3),
            delegation(4, 9, 300, 400, 2),
            delegation(5, 2, 0, 400, 11),
            schedule(1, 0, 400, 10, "independent"),
        ];
        let live_at = |block| live(&schedules, Some(9), block);
        assert_eq!(
            live_at(100),
            Live {
                own_burn: 7,
                delegated_burn: 3,
                delegators: 1,
                delegated_budget: 3 * 400 + 2 * 400,
                network_burn: 7 + 3 + 10,
            }
        );
        // The bot stopped serving: its delegate still burns, ineligibly.
        let later = live_at(350);
        assert_eq!(
            (later.own_burn, later.delegated_burn, later.delegators),
            (0, 5, 2)
        );
        assert_eq!(later.network_burn, 10);
        assert_eq!(live(&schedules, None, 100).delegated_budget, 0);
    }
}
