//! The loop that runs both bots: every few seconds it reads the
//! playground's state and lets each bot act on it; once a minute it records
//! a telemetry sample. Nothing here stops the loop: failures are journaled
//! (once per run of identical ones) and retried on the next poll.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::{
    config::Config,
    journal::{Sample, Submission},
    now_ms,
    playground::{self, ContributionRequest, MinterRequest, Playground},
    strategy::{self, Live, Pace, Plan, Window},
    Store,
};

pub const BOTS: [&str; 2] = ["clock", "minimax"];

const POLL: Duration = Duration::from_secs(5);
const SAMPLE_EVERY_MS: i64 = 60_000;

/// The minimax bot's going rate forgets half of itself in an hour.
const REFERENCE_HALF_LIFE: Duration = Duration::from_secs(3_600);

/// A run of identical failures: journaled when it starts and when it ends.
#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub message: String,
    pub count: u64,
    /// Unix milliseconds.
    pub since: i64,
}

/// What the control page shows of the latest poll.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub started_at: i64,
    pub revision: Option<String>,
    pub playground: String,
    pub polled_at: Option<i64>,
    pub height: Option<i64>,
    /// Ledgers seen: one more each time the playground's ledger is reset.
    pub epoch: i64,
    pub failure: Option<Failure>,
    pub bots: Vec<BotStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BotStatus {
    pub bot: &'static str,
    pub address: String,
    pub user_id: Option<i64>,
    pub pending: Option<Submission>,
    pub failure: Option<Failure>,
    pub sample: Option<Sample>,
    pub live: Live,
    pub pace: Option<Pace>,
    pub reference_price: Option<f64>,
    pub best: Option<Window>,
}

/// A bot's sample of one poll and what it reports now.
type Observed = (Sample, Live);

struct Bot {
    name: &'static str,
    address: String,
    user_id: Option<i64>,
    /// The name and commission last registered in this ledger.
    profile: Option<(String, Option<i64>)>,
    pending: Option<Submission>,
    failure: Option<Failure>,
    pace: Option<Pace>,
    reference_price: Option<f64>,
    best: Option<Window>,
}

pub struct Runner {
    store: Store,
    playground: Playground,
    config: Arc<RwLock<Config>>,
    status: Arc<Mutex<Status>>,
    epoch: i64,
    last_height: Option<i64>,
    last_sample: i64,
    failure: Option<Failure>,
    bots: Vec<Bot>,
    sequence: u64,
}

/// Journals an event and prints it for the service log. A journal failure
/// is printed and otherwise ignored: the bots keep running without it.
pub async fn log(
    store: &Store,
    bot: Option<&'static str>,
    kind: &'static str,
    height: Option<i64>,
    message: String,
    data: Option<serde_json::Value>,
) {
    println!("{} {kind}: {message}", bot.unwrap_or("bots"));
    let result = store
        .call(move |journal| journal.log(now_ms(), bot, kind, height, &message, data.as_ref()))
        .await;
    if let Err(error) = result {
        eprintln!("Not journaled: {error}");
    }
}

async fn fail(
    store: &Store,
    slot: &mut Option<Failure>,
    bot: Option<&'static str>,
    height: Option<i64>,
    message: String,
) {
    match slot {
        Some(failure) if failure.message == message => failure.count += 1,
        _ => {
            log(store, bot, "error", height, message.clone(), None).await;
            *slot = Some(Failure {
                message,
                count: 1,
                since: now_ms(),
            });
        }
    }
}

async fn recover(
    store: &Store,
    slot: &mut Option<Failure>,
    bot: Option<&'static str>,
    height: Option<i64>,
) {
    if let Some(failure) = slot.take() {
        let seconds = (now_ms() - failure.since) / 1_000;
        log(
            store,
            bot,
            "recovered",
            height,
            format!(
                "Recovered after {} failed attempt(s) over {seconds} s: {}",
                failure.count, failure.message
            ),
            Some(json!({ "attempts": failure.count, "since": failure.since })),
        )
        .await;
    }
}

impl Runner {
    pub async fn new(
        store: Store,
        playground: Playground,
        addresses: [String; 2],
        config: Arc<RwLock<Config>>,
        status: Arc<Mutex<Status>>,
    ) -> Result<Self, String> {
        let (epoch, last_height, pending) = store
            .call(|journal| {
                Ok((
                    journal.setting::<i64>("epoch")?.unwrap_or(0),
                    journal.setting::<i64>("last_height")?,
                    [journal.pending(BOTS[0])?, journal.pending(BOTS[1])?],
                ))
            })
            .await?;
        let bots = BOTS
            .into_iter()
            .zip(addresses)
            .zip(pending)
            .map(|((name, address), pending)| Bot {
                name,
                address,
                user_id: None,
                profile: None,
                pending,
                failure: None,
                pace: None,
                reference_price: None,
                best: None,
            })
            .collect();
        Ok(Self {
            store,
            playground,
            config,
            status,
            epoch,
            last_height,
            last_sample: 0,
            failure: None,
            bots,
            sequence: 0,
        })
    }

    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(POLL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.poll().await;
        }
    }

    async fn poll(&mut self) {
        let now = now_ms();
        let state = match self.playground.state().await {
            Ok(state) => {
                recover(&self.store, &mut self.failure, None, Some(state.height)).await;
                state
            }
            Err(error) => {
                fail(
                    &self.store,
                    &mut self.failure,
                    None,
                    self.last_height,
                    error.to_string(),
                )
                .await;
                self.publish(now, None);
                return;
            }
        };
        if self.last_height.is_some_and(|last| state.height < last) {
            self.reset(state.height).await;
        }
        if self.last_height != Some(state.height) {
            self.last_height = Some(state.height);
            let height = state.height;
            if let Err(error) = self
                .store
                .call(move |journal| journal.set_setting("last_height", &height))
                .await
            {
                eprintln!("Not journaled: {error}");
            }
        }
        let config = self.config.read().await.clone();
        for index in 0..self.bots.len() {
            self.step(index, &state, &config, now).await;
        }
        let mut samples = Vec::with_capacity(self.bots.len());
        for bot in &self.bots {
            samples.push(self.observe(bot, &state, &config, now).await);
        }
        if now - self.last_sample >= SAMPLE_EVERY_MS {
            self.last_sample = now;
            let rows: Vec<Sample> = samples
                .iter()
                .flatten()
                .map(|(sample, _)| sample.clone())
                .collect();
            if let Err(error) = self
                .store
                .call(move |journal| {
                    rows.iter()
                        .try_for_each(|sample| journal.add_sample(sample))
                })
                .await
            {
                eprintln!("Not journaled: {error}");
            }
        }
        self.publish(now, Some((state.height, samples)));
    }

    /// The playground's ledger went back: a new demo. The bots register
    /// again, and a submission still pending was meant for the old ledger.
    async fn reset(&mut self, height: i64) {
        let last = self.last_height.unwrap_or_default();
        self.epoch += 1;
        let epoch = self.epoch;
        let dropped: Vec<String> = self
            .bots
            .iter_mut()
            .filter_map(|bot| {
                bot.user_id = None;
                bot.profile = None;
                bot.reference_price = None;
                bot.pending.take().map(|pending| pending.request_id)
            })
            .collect();
        let result = self
            .store
            .call(move |journal| {
                journal.set_setting("epoch", &epoch)?;
                dropped.iter().try_for_each(|request_id| {
                    journal.resolve_submission(request_id, now_ms(), Err("ledger reset"))
                })
            })
            .await;
        if let Err(error) = result {
            eprintln!("Not journaled: {error}");
        }
        log(
            &self.store,
            None,
            "reset",
            Some(height),
            format!("The playground's ledger restarted: block {height}, was {last}. Counting ledger {epoch} from here."),
            Some(json!({ "epoch": epoch, "previous_height": last })),
        )
        .await;
    }

    async fn step(&mut self, index: usize, state: &playground::State, config: &Config, now: i64) {
        let (store, playground, epoch) = (&self.store, &self.playground, self.epoch);
        let height = Some(state.height);
        let bot = &mut self.bots[index];
        let (name, commission, enabled) = match bot.name {
            "clock" => (&config.clock.name, None, config.clock.enabled),
            _ => {
                let minimax = &config.minimax;
                (
                    &minimax.name,
                    minimax.enabled.then_some(minimax.commission_bps),
                    minimax.enabled,
                )
            }
        };

        let profile = (name.clone(), commission);
        if bot.user_id.is_none() || bot.profile.as_ref() != Some(&profile) {
            let request = MinterRequest {
                address: &bot.address,
                name,
                service_commission_bps: commission,
            };
            match playground.minter(&request).await {
                Ok(minter) => {
                    recover(store, &mut bot.failure, Some(bot.name), height).await;
                    let rate = commission
                        .map(|bps| format!(", commission {}%", bps as f64 / 100.0))
                        .unwrap_or_default();
                    log(
                        store,
                        Some(bot.name),
                        "profile",
                        height,
                        format!("Registered as {name} (user #{}){rate}.", minter.user_id),
                        Some(json!({ "user_id": minter.user_id, "name": name, "commission_bps": commission, "address": bot.address })),
                    )
                    .await;
                    bot.user_id = Some(minter.user_id);
                    bot.profile = Some(profile);
                }
                Err(error) => {
                    fail(
                        store,
                        &mut bot.failure,
                        Some(bot.name),
                        height,
                        error.to_string(),
                    )
                    .await;
                    return;
                }
            }
        }

        if bot.pending.is_none() && enabled {
            let plan = match bot.name {
                "clock" => strategy::clock_in(
                    &state.chart_contributions,
                    state.height,
                    config.clock.burn_per_block,
                    config.clock.duration,
                    config.clock.lookahead_blocks,
                ),
                _ => {
                    let minimax = &config.minimax;
                    let since = minimax.starts_at.saturating_mul(1_000);
                    let committed = match store
                        .call(move |journal| journal.committed("minimax", epoch, since))
                        .await
                    {
                        Ok(committed) => committed,
                        Err(error) => {
                            fail(store, &mut bot.failure, Some(bot.name), height, error).await;
                            return;
                        }
                    };
                    let pace = strategy::pace(
                        minimax.budget_sats,
                        committed,
                        minimax.starts_at,
                        minimax.ends_at,
                        now / 1_000,
                    );
                    bot.pace = Some(pace);
                    bot.best = (pace.chunk >= strategy::MIN_BUDGET)
                        .then(|| {
                            strategy::cheapest_window(
                                &state.chart_contributions,
                                bot.user_id.unwrap_or_default(),
                                state.height,
                                pace.chunk,
                                minimax.lookahead_blocks,
                            )
                        })
                        .flatten();
                    bot.best.as_ref().and_then(|window| {
                        let weight = 1.0
                            - 0.5f64.powf(POLL.as_secs_f64() / REFERENCE_HALF_LIFE.as_secs_f64());
                        let reference = bot.reference_price.map_or(window.price, |reference| {
                            reference + weight * (window.price - reference)
                        });
                        bot.reference_price = Some(reference);
                        strategy::should_spend(pace, committed, window.price, reference).then(
                            || Plan {
                                budget_sats: pace.chunk,
                                duration: window.duration,
                                delay_blocks: window.delay_blocks,
                                reason: json!({
                                    "price_sat_per_flm": window.price,
                                    "reference_price": reference,
                                    "expected_flm": window.expected_flm,
                                    "burn_per_block": window.burn_per_block,
                                    "allowance_sats": pace.allowance,
                                    "committed_sats": committed,
                                }),
                            },
                        )
                    })
                }
            };
            if let Some(plan) = plan {
                self.sequence += 1;
                let submission = Submission {
                    request_id: format!("bot-{}-{now}-{}", bot.name, self.sequence),
                    bot: bot.name.into(),
                    epoch,
                    created_at: now,
                    budget_sats: plan.budget_sats,
                    duration: plan.duration,
                    delay_blocks: plan.delay_blocks,
                    mode: if bot.name == "clock" {
                        "independent"
                    } else {
                        "service"
                    }
                    .into(),
                    reason: plan.reason,
                };
                let record = submission.clone();
                // Not sent unless recorded: an unrecorded contribution would escape the budget.
                if let Err(error) = store
                    .call(move |journal| journal.add_submission(&record))
                    .await
                {
                    fail(store, &mut bot.failure, Some(bot.name), height, error).await;
                    return;
                }
                bot.pending = Some(submission);
            }
        }

        let Some(submission) = bot.pending.clone() else {
            return;
        };
        let request = ContributionRequest {
            address: &bot.address,
            budget_sats: submission.budget_sats,
            duration: submission.duration,
            delay_blocks: submission.delay_blocks,
            request_id: &submission.request_id,
            mode: &submission.mode,
        };
        let outcome = match playground.contribute(&request).await {
            Ok(submitted) => Ok(submitted),
            Err(playground::Error::Rejected(status, message)) => {
                Err(format!("{}: {message}", status.as_u16()))
            }
            Err(error) => {
                fail(
                    store,
                    &mut bot.failure,
                    Some(bot.name),
                    height,
                    error.to_string(),
                )
                .await;
                return;
            }
        };
        recover(store, &mut bot.failure, Some(bot.name), height).await;
        bot.pending = None;
        let request_id = submission.request_id.clone();
        let resolved = outcome
            .as_ref()
            .map(|submitted| (submitted.id, submitted.start_block))
            .map_err(String::clone);
        if let Err(error) = store
            .call(move |journal| {
                journal.resolve_submission(
                    &request_id,
                    now_ms(),
                    resolved.as_ref().map(|r| *r).map_err(String::as_str),
                )
            })
            .await
        {
            eprintln!("Not journaled: {error}");
        }
        let (kind, message, data) = match outcome {
            Ok(submitted) => (
                "submitted",
                format!(
                    "Contribution #{}: {} sat over {} blocks from block {}, {}.",
                    submitted.id,
                    submission.budget_sats,
                    submission.duration,
                    submitted.start_block,
                    submission.mode
                ),
                json!({ "contribution_id": submitted.id, "start_block": submitted.start_block, "submission": submission }),
            ),
            Err(message) => (
                "rejected",
                format!("Refused: {message}"),
                json!({ "submission": submission }),
            ),
        };
        log(store, Some(bot.name), kind, height, message, Some(data)).await;
    }

    /// The bot's sample of this poll, and what it reports now; `None`
    /// before it has a user ID in this ledger.
    async fn observe(
        &self,
        bot: &Bot,
        state: &playground::State,
        config: &Config,
        now: i64,
    ) -> Option<Observed> {
        let user_id = bot.user_id?;
        let minimax = bot.name == "minimax";
        let since = if minimax {
            config.minimax.starts_at.saturating_mul(1_000)
        } else {
            0
        };
        let (name, epoch) = (bot.name, self.epoch);
        let committed_sats = self
            .store
            .call(move |journal| journal.committed(name, epoch, since))
            .await
            .ok()?;
        let live = strategy::live(&state.chart_contributions, Some(user_id), state.height);
        let summary = state
            .minters
            .iter()
            .find(|minter| minter.address == bot.address);
        let sample = Sample {
            at: now,
            bot: bot.name.into(),
            height: state.height,
            committed_sats,
            budget_sats: minimax.then_some(config.minimax.budget_sats),
            allowance_sats: minimax.then(|| {
                strategy::pace(
                    config.minimax.budget_sats,
                    committed_sats,
                    config.minimax.starts_at,
                    config.minimax.ends_at,
                    now / 1_000,
                )
                .allowance
            }),
            burned_sats: summary.map_or(0, |summary| summary.burned_sats),
            fees_sats: summary.map_or(0, |summary| summary.fees_sats),
            reward_sparks: summary.map_or(0, |summary| summary.reward_sparks),
            own_burn: live.own_burn,
            delegated_burn: live.delegated_burn,
            delegated_budget_sats: live.delegated_budget,
            delegators: live.delegators,
            network_burn: live.network_burn,
        };
        Some((sample, live))
    }

    fn publish(&self, now: i64, observed: Option<(i64, Vec<Option<Observed>>)>) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.polled_at = Some(now);
        status.epoch = self.epoch;
        status.failure = self.failure.clone();
        let samples = match observed {
            Some((height, samples)) => {
                status.height = Some(height);
                samples
            }
            None => status
                .bots
                .iter()
                .map(|bot| bot.sample.clone().map(|sample| (sample, bot.live)))
                .collect(),
        };
        status.bots = self
            .bots
            .iter()
            .zip(samples.into_iter().chain(std::iter::repeat(None)))
            .map(|(bot, observed)| BotStatus {
                bot: bot.name,
                address: bot.address.clone(),
                user_id: bot.user_id,
                pending: bot.pending.clone(),
                failure: bot.failure.clone(),
                live: observed.as_ref().map(|(_, live)| *live).unwrap_or_default(),
                sample: observed.map(|(sample, _)| sample),
                pace: bot.pace,
                reference_price: bot.reference_price,
                best: bot.best.clone(),
            })
            .collect();
    }
}
