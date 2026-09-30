//! The results file, the scratch files the benchmarks leave for the report,
//! and what the report derives from them.
//!
//! A results file holds everything the charts and tables of report 1 use,
//! so a chart can be redrawn from a committed file without re-running
//! anything. Derived numbers — rates, shares, block capacity, the gas
//! predictions, a warm send, generator setup — are computed when drawing
//! and never stored.

use serde::{Deserialize, Serialize};

use crate::cpu::Cpu;
use crate::fixtures::DecodeMethod;

/// The results file format.
pub const SCHEMA: u32 = 2;

/// One gas is about this much verifier work (`docs/flamevm.md`, gas
/// calibration).
pub const NS_PER_GAS: f64 = 100.0;

/// `GAS_R1CS_ITEM` in `flamevm/src/vm.rs`: what the schedule charges per
/// R1CS item. `mix` charges it for `(m + n)²` items.
pub const GAS_R1CS_ITEM: u64 = 120;

/// One run of report 1, as committed under
/// `docs/benchmarks/results/transactions/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Results {
    pub schema: u32,
    pub run: Run,
    pub machine: Machine,
    pub shapes: Vec<ShapeResult>,
    /// Transfers tried beyond the shapes, to find the proof's capacity.
    pub over_capacity: Vec<CapacityAttempt>,
    pub limits: ChainLimits,
    /// Every criterion median the file uses, with its confidence interval.
    pub estimates: Vec<Estimate>,
}

/// When and from what the numbers were taken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// `YYYY-MM-DD`, UTC.
    pub date: String,
    /// `HEAD` when the report ran.
    pub commit: String,
    /// Whether tracked files differed from `HEAD`.
    pub dirty: bool,
    /// `rustc --version`.
    pub rustc: String,
    /// The documented commands that produce the numbers.
    pub commands: Vec<String>,
    /// How long each benchmark ran, fixtures included.
    pub durations: Vec<BenchTime>,
}

/// One benchmark's wall time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchTime {
    pub bench: String,
    pub secs: f64,
}

/// The machine and its conditions during a run: what each bench records
/// in its scratch file, and what the results file keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub cpu_model: String,
    /// Every CPU, fast cores first. The benchmarks run on the first.
    pub cpu_order: Vec<Cpu>,
    pub governor: Option<String>,
    pub energy_performance_preference: Option<String>,
    /// `/sys/firmware/acpi/platform_profile`.
    pub platform_profile: Option<String>,
    pub power_source: Option<String>,
}

/// One shape's measured and recorded numbers. Times are criterion medians,
/// in nanoseconds, unless they say otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeResult {
    pub id: String,
    pub label: String,
    pub inputs: usize,
    pub outputs: usize,
    /// The fee it pays, in sparks. A fee is one more input to `mix`.
    pub fee: u64,
    pub bytes: usize,
    pub gas_used: u64,
    /// `TxMetrics.multiplications`: the proof's gates.
    pub multiplications: usize,
    pub decode_method: DecodeMethod,
    pub verify_ns: f64,
    pub decode_ns: f64,
    pub signature_ns: f64,
    /// A synthetic R1CS verification with the same multiplier count.
    pub r1cs_estimate_ns: f64,
    /// One Utreexo membership check, per forest size.
    pub utreexo: Vec<UtreexoTime>,
    /// Benchmark 2: sending it.
    pub send: SendResult,
}

/// One Utreexo membership check in a forest of `leaves`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UtreexoTime {
    pub leaves: u64,
    /// Hashes on the path: log2 of `leaves`.
    pub depth: u32,
    pub ns: f64,
}

/// What building and signing one transaction of a shape costs its sender.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendResult {
    /// `InputSpec::confidential` for every input.
    pub prepare_ns: f64,
    /// `build_transfer`: sealing the notes, running the prover, proving.
    pub build_ns: f64,
    /// Everything `sign` computes.
    pub sign_ns: f64,
    /// `block_tx` and its encoding.
    pub package_ns: f64,
    /// `open_note` of one output.
    pub open_note_ns: f64,
    /// Proving a synthetic R1CS with the same gate count.
    pub prove_estimate_ns: f64,
    /// Heap in use at its highest during one send in a warm process, beyond
    /// what was in use before it.
    pub warm_peak_bytes: u64,
    /// Sends in fresh processes.
    pub fresh: FreshProcess,
}

/// Two sends in each of `runs` fresh processes, pinned to the benchmarks'
/// CPU. Times are the medians over the runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshProcess {
    pub runs: usize,
    /// The process's first send, which builds the prover's generators.
    pub first_ns: f64,
    /// Its second send, right after.
    pub second_ns: f64,
    /// The largest peak heap of a first send over the runs, beyond what
    /// was in use before it: the generator table included.
    pub first_peak_bytes: u64,
}

/// A transfer tried past the proof's capacity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityAttempt {
    pub id: String,
    pub label: String,
    pub inputs: usize,
    pub outputs: usize,
    /// Whether it proved.
    pub proves: bool,
    /// Why not, when it did not.
    pub error: Option<String>,
    /// Its gates, when they could be observed. A transfer that fails to
    /// prove returns no metrics, so this is `None` unless it proved.
    pub multiplications: Option<usize>,
}

/// The consensus limits the report needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainLimits {
    pub max_multiplications: usize,
    pub max_multiplications_per_transaction: usize,
    pub max_transactions: usize,
}

/// One criterion median and its 95% confidence interval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Estimate {
    /// `group/function/parameter`.
    pub id: String,
    pub median_ns: f64,
    pub lower_ns: f64,
    pub upper_ns: f64,
}

/// What the `tx_cost` bench knows about its fixtures, left in
/// `<target>/flamebench/fixtures.json` for the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixturesFile {
    pub shapes: Vec<FixtureShape>,
    /// The forest sizes the Utreexo group measures.
    pub utreexo_leaves: Vec<u64>,
    pub limits: ChainLimits,
    /// The CPU order and conditions while it ran.
    pub machine: Machine,
    /// Its wall time, fixtures included.
    pub elapsed_secs: f64,
}

/// One shape as built.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureShape {
    pub id: String,
    pub label: String,
    pub inputs: usize,
    pub outputs: usize,
    pub fee: u64,
    /// `encoded_size()` of the signed transaction.
    pub bytes: usize,
    pub gas_used: u64,
    pub multiplications: usize,
    pub decode_method: DecodeMethod,
}

/// What the `send` bench measured outside criterion, left in
/// `<target>/flamebench/send.json` for the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendFile {
    /// The shapes as the `send` bench built them. The report checks they
    /// are the ones `tx_cost` built.
    pub shapes: Vec<SendShape>,
    pub over_capacity: Vec<CapacityAttempt>,
    /// The CPU order and conditions while it ran.
    pub machine: Machine,
    /// Its wall time, fixtures and fresh processes included.
    pub elapsed_secs: f64,
}

/// One shape's numbers from outside criterion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendShape {
    pub fixture: FixtureShape,
    pub warm_peak_bytes: u64,
    pub fresh: FreshProcess,
}

impl ShapeResult {
    /// Utreexo per input in the largest measured forest.
    pub fn utreexo_ns(&self) -> f64 {
        self.largest_forest().map_or(0.0, |time| time.ns)
    }

    /// The forest the per-input Utreexo cost is taken from.
    pub fn largest_forest(&self) -> Option<&UtreexoTime> {
        self.utreexo.iter().max_by_key(|time| time.leaves)
    }

    /// One Utreexo check for every input: the fourth bar segment.
    pub fn utreexo_all_inputs_ns(&self) -> f64 {
        self.utreexo_ns() * self.inputs as f64
    }

    /// Verify plus Utreexo for every input: one transaction's whole cost.
    pub fn total_ns(&self) -> f64 {
        self.verify_ns + self.utreexo_ns() * self.inputs as f64
    }

    /// Transactions of this shape one core verifies per second.
    pub fn tps_per_core(&self) -> f64 {
        per_second(self.total_ns())
    }

    /// The R1CS estimate, capped at what verify leaves after the signature.
    pub fn proof_ns(&self) -> f64 {
        self.r1cs_estimate_ns
            .min((self.verify_ns - self.signature_ns).max(0.0))
    }

    /// Verify minus the R1CS estimate minus the signature, clamped to zero.
    pub fn vm_and_other_ns(&self) -> f64 {
        (self.verify_ns - self.r1cs_estimate_ns - self.signature_ns).max(0.0)
    }

    /// Whether the synthetic estimate overshot: the unclamped "VM and other"
    /// share is negative. The signal to cross-check with a profile.
    pub fn estimate_overshot(&self) -> bool {
        self.verify_ns - self.r1cs_estimate_ns - self.signature_ns < 0.0
    }

    /// The proof's share of verify time.
    pub fn proof_share(&self) -> f64 {
        ratio(self.proof_ns(), self.verify_ns)
    }

    /// `mix`'s inputs: the spent contracts, plus one for the fee.
    pub fn mix_inputs(&self) -> usize {
        self.inputs + usize::from(self.fee > 0)
    }

    /// The items `mix` charges R1CS gas for: `(m + n)²`.
    pub fn mix_items(&self) -> u64 {
        let items = (self.mix_inputs() + self.outputs) as u64;
        items * items
    }

    /// The gas this shape would use if `mix` charged 120 per real gate
    /// instead of 120 per `(m + n)²` item. Every other charge stays.
    pub fn gas_per_gate(&self) -> u64 {
        (self.gas_used + GAS_R1CS_ITEM * self.multiplications as u64)
            .saturating_sub(GAS_R1CS_ITEM * self.mix_items())
    }

    /// What the gas schedule predicts verify costs, as it charges today.
    pub fn gas_predicted_ns(&self) -> f64 {
        self.gas_used as f64 * NS_PER_GAS
    }

    /// What it would predict with `mix` charged per real gate.
    pub fn gas_per_gate_predicted_ns(&self) -> f64 {
        self.gas_per_gate() as f64 * NS_PER_GAS
    }

    /// How far a prediction lies from the measurement, as a fraction of
    /// the measurement. Negative when gas underprices the work.
    pub fn gas_error(&self, predicted_ns: f64) -> f64 {
        ratio(predicted_ns, self.verify_ns) - 1.0
    }

    /// Transactions of this shape one block holds: bounded by gates and by
    /// count.
    pub fn per_block(&self, limits: &ChainLimits) -> usize {
        limits
            .max_multiplications
            .checked_div(self.multiplications)
            .unwrap_or(usize::MAX)
            .min(limits.max_transactions)
    }

    /// Payments in a block of this shape: every output but the change.
    pub fn payments_per_block(&self, limits: &ChainLimits) -> usize {
        self.per_block(limits) * self.outputs.saturating_sub(1)
    }

    /// One core's time to verify a full block of this shape: verify plus
    /// Utreexo, per transaction.
    pub fn full_block_ns(&self, limits: &ChainLimits) -> f64 {
        self.per_block(limits) as f64 * self.total_ns()
    }

    /// Prepare, build, sign and package, in a warm process.
    pub fn warm_send_ns(&self) -> f64 {
        let send = &self.send;
        send.prepare_ns + send.build_ns + send.sign_ns + send.package_ns
    }

    /// What a new process pays once, on its first send: the new processes'
    /// median first send minus their median second send.
    pub fn generator_setup_ns(&self) -> f64 {
        (self.send.fresh.first_ns - self.send.fresh.second_ns).max(0.0)
    }

    /// A first send: the median of the new processes' first sends, as
    /// measured.
    pub fn first_send_ns(&self) -> f64 {
        self.send.fresh.first_ns
    }

    /// Sealing every note, estimated as `outputs` × `open_note`: the two do
    /// the same key agreement, transcripts, AES-SIV and commitments.
    pub fn sealing_estimate_ns(&self) -> f64 {
        self.send.open_note_ns * self.outputs as f64
    }

    /// Proving, estimated by the synthetic prove, capped at build.
    pub fn proving_ns(&self) -> f64 {
        self.send.prove_estimate_ns.min(self.send.build_ns)
    }

    /// Build minus the proving and sealing estimates, clamped to zero: the
    /// prover's run of the script, and everything else in `build_transfer`.
    pub fn prover_run_ns(&self) -> f64 {
        (self.send.build_ns - self.send.prove_estimate_ns - self.sealing_estimate_ns()).max(0.0)
    }

    /// Whether the estimates overshot build: the unclamped prover run is
    /// negative.
    pub fn send_estimate_overshot(&self) -> bool {
        self.send.build_ns - self.send.prove_estimate_ns - self.sealing_estimate_ns() < 0.0
    }

    /// Build minus the proving estimate: the prover run and sealing, the
    /// middle segment of a send.
    pub fn build_other_ns(&self) -> f64 {
        (self.send.build_ns - self.proving_ns()).max(0.0)
    }

    /// Prepare, sign and package: the last segment of a send.
    pub fn around_build_ns(&self) -> f64 {
        self.send.prepare_ns + self.send.sign_ns + self.send.package_ns
    }

    /// The proving estimate's share of a warm send.
    pub fn proving_share(&self) -> f64 {
        ratio(self.proving_ns(), self.warm_send_ns())
    }
}

impl Results {
    /// The first shape: the everyday payment, 1 → 2.
    pub fn payment(&self) -> &ShapeResult {
        &self.shapes[0]
    }

    /// The results file's name: `<date>-<cpu slug>-<short commit>.json`.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}-{}.json",
            self.run.date,
            crate::cpu::cpu_slug(&self.machine.cpu_model),
            short_commit(&self.run.commit)
        )
    }

    /// The most outputs a transaction proved with: a shape, or a transfer
    /// tried past the shapes that proved.
    pub fn most_outputs(&self) -> Option<usize> {
        self.shapes
            .iter()
            .map(|shape| shape.outputs)
            .chain(
                self.over_capacity
                    .iter()
                    .filter(|attempt| attempt.proves)
                    .map(|attempt| attempt.outputs),
            )
            .max()
    }

    /// The fewest outputs a transfer failed to prove with, if one did.
    pub fn fewest_failing_outputs(&self) -> Option<usize> {
        self.over_capacity
            .iter()
            .filter(|attempt| !attempt.proves)
            .map(|attempt| attempt.outputs)
            .min()
    }

    /// Checks what the charts divide by and index into.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "results schema {} is not {SCHEMA}, the one this report reads",
                self.schema
            ));
        }
        if self.shapes.is_empty() {
            return Err("the results hold no shapes".into());
        }
        if self.machine.cpu_order.is_empty() {
            return Err("the results hold no CPU order".into());
        }
        let times = self.shapes.iter().flat_map(|shape| {
            let send = &shape.send;
            [
                shape.verify_ns,
                shape.decode_ns,
                shape.signature_ns,
                shape.r1cs_estimate_ns,
                send.prepare_ns,
                send.build_ns,
                send.sign_ns,
                send.package_ns,
                send.open_note_ns,
                send.prove_estimate_ns,
                send.fresh.first_ns,
                send.fresh.second_ns,
            ]
            .into_iter()
            .chain(shape.utreexo.iter().map(|time| time.ns))
        });
        let estimates = self
            .estimates
            .iter()
            .flat_map(|e| [e.median_ns, e.lower_ns, e.upper_ns]);
        if times
            .chain(estimates)
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err("the results hold a negative or non-finite number".into());
        }
        if self
            .shapes
            .iter()
            .any(|shape| shape.verify_ns <= 0.0 || shape.send.build_ns <= 0.0)
        {
            return Err("a shape has no verify or build time".into());
        }
        if self.shapes.iter().any(|shape| shape.outputs == 0) {
            return Err("a shape has no outputs".into());
        }
        Ok(())
    }
}

/// The first seven characters of a commit hash.
pub fn short_commit(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// Events per second, given nanoseconds per event.
pub fn per_second(ns: f64) -> f64 {
    ratio(1e9, ns)
}

/// `a / b`, or zero when `b` is not positive.
pub fn ratio(a: f64, b: f64) -> f64 {
    if b > 0.0 {
        a / b
    } else {
        0.0
    }
}
