//! Cost attribution over a derived trajectory (ADR-015).
//!
//! Nothing here is measured. Every figure in this module is a
//! measured quantity from [`crate::trajectory::TrajectoryMetrics`]
//! multiplied by a rate the caller declared, which is why cost lives
//! outside `Report` (D1): a serialized report contains measurements,
//! and a reader cannot mistake a declared rate for one.
//!
//! The validity domain is single-tenant profiling runs. The run
//! window is not the invoice: provisioning, model load and post-run
//! idle are billed and are not in it, providers bill by the minute or
//! the hour, and no figure here is reconciled against what was
//! actually paid. See ADR-015 D7.

use serde::{Deserialize, Serialize};

use crate::trajectory::{StepKind, TrajectoryMetrics};

/// Nanoseconds in one hour.
const NS_PER_HOUR: f64 = 3_600_000_000_000.0;
/// Millijoules in one kilowatt-hour.
const MJ_PER_KWH: f64 = 3_600_000_000.0;

/// Which cost is being derived, and at what rate (ADR-015 D2, D3).
///
/// One basis per derivation. There is no total that sums them: on a
/// rented node the energy is already inside the hourly price, so
/// adding the two would double-count. A consumer that wants both
/// computes both and presents them as two answers to two different
/// questions.
///
/// The rate is carried in the variant rather than passed alongside
/// it, so a result is never separable from the assumption that made
/// it, and a per-kWh price cannot reach an occupancy derivation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "basis")]
pub enum CostBasis {
    /// Node rented by wall-clock time; energy is already priced in.
    Occupancy {
        /// Declared price of the whole node, per hour.
        usd_per_hour: f64,
    },
    /// Hardware owned; electricity metered separately.
    Energy {
        /// Declared electricity price, per kilowatt-hour.
        usd_per_kwh: f64,
    },
}

/// Cost of one trajectory step on the declared basis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepCost {
    /// Driver-assigned step identifier.
    pub step_id: u64,
    /// Whether the step was an LLM call or a tool execution.
    pub kind: StepKind,
    /// Cost attributed to this step's window.
    pub usd: f64,
    /// Cost per million generation tokens produced in this step.
    /// `None` for tool steps, for steps with no scraped phase
    /// timeline, and for zero generation tokens (ADR-015 D6): a step
    /// that produced no tokens has no cost per token, it does not
    /// have a cost per token of zero.
    pub usd_per_million_tokens: Option<f64>,
}

/// Cost attribution over a whole trajectory on one basis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryCost {
    /// The basis and rate that produced every figure below (D3).
    pub basis: CostBasis,
    /// Cost of the whole run window.
    pub run_usd: f64,
    /// Summed cost of the kept step windows.
    pub attributed_usd: f64,
    /// Attributed cost spent in steps that generated tokens.
    ///
    /// Exactly `attributed_usd` minus [`Self::tool_usd`]: the two
    /// partition the kept step windows by kind and introduce no new
    /// price. What they do not partition is `run_usd` — a step
    /// dropped for falling outside the run window contributes to
    /// neither, and its duration stays in the residual.
    pub llm_usd: f64,
    /// Attributed cost spent in steps that generated no tokens.
    ///
    /// On an agentic trajectory this is what the GPU costs while the
    /// agent is executing a tool rather than serving tokens. It is
    /// paid on the occupancy basis because the node is held, and on
    /// the energy basis because the device still draws power.
    pub tool_usd: f64,
    /// Cost of the run outside any kept step window. On the
    /// occupancy basis this is paid: the node is powered during model
    /// load, between steps, and while the driver thinks (D5).
    pub unattributed_usd: f64,
    /// Per-step figures, in window order.
    pub steps: Vec<StepCost>,
    /// Whole-run cost per million generation tokens, computed over
    /// `run_usd` and not over `attributed_usd`: the residual was paid
    /// and excluding it would understate the price of a token.
    /// `None` when the run produced no generation tokens.
    pub trajectory_usd_per_million_tokens: Option<f64>,
}

/// Derives [`TrajectoryCost`] from measured trajectory figures and a
/// declared rate (ADR-015).
///
/// Pure: it reads no clock, no environment and no price list. The
/// integration window is the one already fixed by the trajectory
/// layer, so occupancy and energy describe the same interval (D4).
///
/// Returns `None` when the basis has no quantity to price: a zero run
/// duration (which is also what a pre-ADR-015 report deserializes to)
/// on the occupancy basis, or zero measured energy on the energy
/// basis. Absence of the underlying measurement is withheld, not
/// priced at zero.
pub fn derive_cost(t: &TrajectoryMetrics, basis: CostBasis) -> Option<TrajectoryCost> {
    let (run_usd, attributed_usd, unattributed_usd, step_usd): (f64, f64, f64, Vec<f64>) =
        match basis {
            CostBasis::Occupancy { usd_per_hour } => {
                if t.run_duration_ns == 0 {
                    return None;
                }
                let price = |ns: u64| usd_per_hour * (ns as f64) / NS_PER_HOUR;
                let steps: Vec<f64> = t
                    .steps
                    .iter()
                    .map(|s| price(s.end_elapsed_ns.saturating_sub(s.start_elapsed_ns)))
                    .collect();
                (
                    price(t.run_duration_ns),
                    steps.iter().sum(),
                    price(t.unattributed_duration_ns),
                    steps,
                )
            }
            CostBasis::Energy { usd_per_kwh } => {
                if t.total_energy_mj == 0 {
                    return None;
                }
                let price = |mj: u64| usd_per_kwh * (mj as f64) / MJ_PER_KWH;
                let steps: Vec<f64> = t.steps.iter().map(|s| price(s.energy_mj)).collect();
                (
                    price(t.total_energy_mj),
                    steps.iter().sum(),
                    price(t.unattributed_energy_mj),
                    steps,
                )
            }
        };

    let steps: Vec<StepCost> = t
        .steps
        .iter()
        .zip(step_usd)
        .map(|(s, usd)| StepCost {
            step_id: s.step_id,
            kind: s.kind,
            usd,
            usd_per_million_tokens: match (s.kind, s.generation_tokens_delta) {
                (StepKind::Tool, _) | (_, None) | (_, Some(0)) => None,
                (StepKind::LlmCall, Some(tokens)) => Some(usd * 1_000_000.0 / tokens as f64),
            },
        })
        .collect();

    // Partition of the kept windows by kind. Summed from the same
    // per-step figures rendered above, so the identity
    // `llm_usd + tool_usd == attributed_usd` holds by construction on
    // both bases rather than by a second derivation.
    let llm_usd: f64 = steps
        .iter()
        .filter(|s| s.kind == StepKind::LlmCall)
        .map(|s| s.usd)
        .sum();
    let tool_usd: f64 = steps
        .iter()
        .filter(|s| s.kind == StepKind::Tool)
        .map(|s| s.usd)
        .sum();
    let trajectory_usd_per_million_tokens = if t.total_generation_tokens == 0 {
        None
    } else {
        Some(run_usd * 1_000_000.0 / t.total_generation_tokens as f64)
    };

    Some(TrajectoryCost {
        basis,
        run_usd,
        attributed_usd,
        llm_usd,
        tool_usd,
        unattributed_usd,
        steps,
        trajectory_usd_per_million_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::DroppedStep;

    /// One second in nanoseconds.
    const S: u64 = 1_000_000_000;

    fn step_metrics(
        step_id: u64,
        kind: StepKind,
        start_ns: u64,
        end_ns: u64,
        energy_mj: u64,
        generation_tokens_delta: Option<u64>,
    ) -> crate::trajectory::StepMetrics {
        crate::trajectory::StepMetrics {
            step_id,
            kind,
            start_elapsed_ns: start_ns,
            end_elapsed_ns: end_ns,
            samples_in_window: 2,
            energy_mj,
            generation_tokens_delta,
            prompt_tokens_delta: None,
            cache_hits_delta: None,
            cache_queries_delta: None,
            tokens_per_joule: None,
            cache_hit_rate: None,
        }
    }

    /// 4 s run carrying 600_000 mJ and 400 generation tokens: one LLM
    /// step (1 s, 150_000 mJ, 100 tokens) and one tool step (1 s,
    /// 150_000 mJ, no tokens). The remaining 2 s and 300_000 mJ are
    /// the residuals.
    fn trajectory() -> TrajectoryMetrics {
        TrajectoryMetrics {
            steps: vec![
                step_metrics(1, StepKind::LlmCall, 0, S, 150_000, Some(100)),
                step_metrics(2, StepKind::Tool, 2 * S, 3 * S, 150_000, None),
            ],
            total_energy_mj: 600_000,
            total_generation_tokens: 400,
            trajectory_tokens_per_joule: Some(400.0 / 600.0),
            llm_energy_mj: 150_000,
            tool_energy_mj: 150_000,
            unattributed_energy_mj: 300_000,
            run_duration_ns: 4 * S,
            unattributed_duration_ns: 2 * S,
            dropped_steps: Vec::<DroppedStep>::new(),
        }
    }

    /// The partition is exact on both bases: it re-sums the same
    /// per-step figures the report renders, rather than deriving a
    /// second time from the trajectory.
    #[test]
    fn llm_and_tool_partition_the_attributed_cost() {
        for basis in [
            CostBasis::Occupancy { usd_per_hour: 1.0 },
            CostBasis::Energy { usd_per_kwh: 0.25 },
        ] {
            let c = derive_cost(&trajectory(), basis).expect("priceable");
            assert!(
                (c.llm_usd + c.tool_usd - c.attributed_usd).abs() < 1e-12,
                "{basis:?}: {} + {} != {}",
                c.llm_usd,
                c.tool_usd,
                c.attributed_usd
            );
        }
    }

    /// What the partition is for. The two steps hold equal energy but
    /// the tool one generated nothing, so on the energy basis half the
    /// attributed cost bought no tokens. A single `attributed_usd`
    /// cannot say that.
    #[test]
    fn the_tool_share_is_the_cost_that_bought_no_tokens() {
        let c = derive_cost(&trajectory(), CostBasis::Energy { usd_per_kwh: 0.25 })
            .expect("energy is non-zero");
        assert!((c.tool_usd / c.attributed_usd - 0.5).abs() < 1e-12);
        assert!((c.llm_usd - c.tool_usd).abs() < 1e-12);
    }

    /// A trajectory with no tool steps prices its whole attributed
    /// cost as generating. Zero here is measured, not withheld: the
    /// steps exist and none of them is a tool.
    #[test]
    fn a_trajectory_without_tools_has_no_tool_cost() {
        let mut t = trajectory();
        t.steps.retain(|s| s.kind == StepKind::LlmCall);
        t.tool_energy_mj = 0;
        let c = derive_cost(&t, CostBasis::Occupancy { usd_per_hour: 1.0 }).expect("priceable");
        assert_eq!(c.tool_usd, 0.0);
        assert!((c.llm_usd - c.attributed_usd).abs() < 1e-12);
    }

    /// Hand-computed from the fixture: $1.00/h over 4 s is
    /// 4/3600 dollars; the LLM step's 1 s is 1/3600.
    #[test]
    fn occupancy_prices_wall_clock_time() {
        let c = derive_cost(&trajectory(), CostBasis::Occupancy { usd_per_hour: 1.0 })
            .expect("run duration is non-zero");
        assert!((c.run_usd - 4.0 / 3600.0).abs() < 1e-12);
        assert!((c.steps[0].usd - 1.0 / 3600.0).abs() < 1e-12);
        assert!((c.unattributed_usd - 2.0 / 3600.0).abs() < 1e-12);
        // Attributed plus residual recovers the run, on this basis.
        assert!((c.attributed_usd + c.unattributed_usd - c.run_usd).abs() < 1e-12);
    }

    /// Hand-computed from the fixture: $0.50/kWh over 600_000 mJ is
    /// 0.5 * 600_000 / 3_600_000_000 dollars.
    #[test]
    fn energy_prices_measured_joules() {
        let c = derive_cost(&trajectory(), CostBasis::Energy { usd_per_kwh: 0.5 })
            .expect("run energy is non-zero");
        assert!((c.run_usd - 0.5 * 600_000.0 / 3_600_000_000.0).abs() < 1e-15);
        assert!((c.steps[0].usd - 0.5 * 150_000.0 / 3_600_000_000.0).abs() < 1e-15);
        assert!((c.attributed_usd + c.unattributed_usd - c.run_usd).abs() < 1e-15);
    }

    /// D2: the two bases answer different questions and are not
    /// additive. Nothing in the returned type sums them, and on a
    /// rented node the energy is already inside the hourly price.
    #[test]
    fn the_two_bases_are_not_the_same_figure() {
        let t = trajectory();
        let occ = derive_cost(&t, CostBasis::Occupancy { usd_per_hour: 1.0 }).expect("occ");
        let ene = derive_cost(&t, CostBasis::Energy { usd_per_kwh: 0.5 }).expect("ene");
        assert_ne!(occ.run_usd, ene.run_usd);
        // The rate that produced each figure travels with it (D3).
        assert_eq!(occ.basis, CostBasis::Occupancy { usd_per_hour: 1.0 });
        assert_eq!(ene.basis, CostBasis::Energy { usd_per_kwh: 0.5 });
    }

    /// D6: absence is not zero. A tool step has no cost per token; a
    /// step with a scraped zero-token delta has none either.
    #[test]
    fn cost_per_token_is_withheld_not_zeroed() {
        let mut t = trajectory();
        t.steps
            .push(step_metrics(3, StepKind::LlmCall, 3 * S, 4 * S, 1, Some(0)));
        let c = derive_cost(&t, CostBasis::Occupancy { usd_per_hour: 1.0 }).expect("occ");
        assert!(c.steps[0].usd_per_million_tokens.is_some(), "LLM, 100 tok");
        assert_eq!(c.steps[1].usd_per_million_tokens, None, "tool step");
        assert_eq!(c.steps[2].usd_per_million_tokens, None, "zero tokens");
        // The step still has a cost: only the per-token ratio is absent.
        assert!(c.steps[1].usd > 0.0);
    }

    /// A report predating ADR-015 deserializes with
    /// `run_duration_ns == 0` via `serde(default)`. Pricing it would
    /// report a free run; the derivation is withheld instead.
    #[test]
    fn pre_adr015_report_yields_no_occupancy_cost() {
        let mut t = trajectory();
        t.run_duration_ns = 0;
        t.unattributed_duration_ns = 0;
        assert_eq!(
            derive_cost(&t, CostBasis::Occupancy { usd_per_hour: 1.0 }),
            None
        );
        // The energy basis is unaffected: its measurement is present.
        assert!(derive_cost(&t, CostBasis::Energy { usd_per_kwh: 0.5 }).is_some());
    }

    /// D1: cost is not part of the serialized report. The report type
    /// carries no cost field, so a reader cannot mistake a declared
    /// rate for a measurement.
    #[test]
    fn cost_is_absent_from_the_serialized_report() {
        let report = crate::metrics::Report {
            reference_instant_unix_ns: None,
            request_timing: is_core::RequestTiming::new(vec![], 0),
            resource_timeline: None,
            gpu_timeline: None,
            timing: crate::metrics::TimingMetrics {
                token_count: 0,
                ttft_ns: None,
                total_latency_ns: 0,
                tokens_per_second: None,
                inter_token_latency: None,
            },
            resource: None,
            gpu: None,
            efficiency: None,
            kvcache_timeline: None,
            kvcache: None,
            phase_timeline: None,
            phase_energy: None,
            trajectory: Some(trajectory()),
            schema_version: Some(crate::metrics::REPORT_SCHEMA_VERSION),
        };
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("usd"), "no dollar figure reaches the report");
        assert!(
            !json.contains("basis"),
            "no declared rate reaches it either"
        );
        // The measured quantities cost derives from are present.
        assert!(json.contains("run_duration_ns"));
        assert!(json.contains("unattributed_duration_ns"));
    }
}
