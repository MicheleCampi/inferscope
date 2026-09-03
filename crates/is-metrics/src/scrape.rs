//! The scrape loop: reads a `/metrics` endpoint periodically and
//! accumulates a KV-cache timeline.
//!
//! This is the only module in the crate that performs I/O. The pure
//! parsing lives in [`crate::parse`]; this module fetches the body over
//! HTTP, hands it to the parser, and pushes the resulting sample onto a
//! growing timeline at a configured cadence.
//!
//! It mirrors [`is_sysmon`]'s `sample_during` / `sample_gpu_during` in
//! shape (ADR-011): a `tokio::select!` loop with a `biased` cancel arm,
//! `MissedTickBehavior::Skip` so a slow scrape does not burst-catch-up,
//! and the same best-effort contract — a failed scrape is swallowed and
//! the timeline continues. Per ADR-003 each sample's `elapsed_ns` is
//! measured from the shared reference `Instant` the probe also holds.
//!
//! Unlike the GPU sampler, this module does *not* compute a window
//! delta here: it accumulates raw per-tick counter readings, and the
//! window hit rate is derived downstream in `is-report` from the first
//! and last samples (ADR-011). Keeping derivation out of the scrape
//! loop matches `sampler.rs`, which pushes samples and derives nothing.

use std::time::Instant;

use is_core::{KvCacheSample, KvCacheTimeline, PhaseSample, PhaseTimeline, SpecTimeline};
use tokio::sync::oneshot;
use tokio::time::{interval, MissedTickBehavior};

use crate::config::MetricsConfig;
use crate::error::MetricsError;
use crate::parse::{parse_kvcache, parse_phase, parse_spec, SpecReading};

/// Builds the HTTP client used for scraping.
///
/// The timeout is a fixed 5 seconds, independent of the scrape cadence.
/// An earlier design tied it to the sample period, but that is wrong:
/// the period can legitimately be set finer than one HTTP round-trip
/// (e.g. to capture the cache-warming curve), and a localhost `/metrics`
/// round-trip is typically several milliseconds — so a sub-round-trip
/// timeout would fail every scrape. `MissedTickBehavior::Skip` already
/// decouples cadence from scrape duration: a scrape slower than the
/// period makes the loop skip missed ticks, not stall. The timeout only
/// guards against an endpoint that accepts the connection but never
/// responds.
fn build_client() -> Result<reqwest::Client, MetricsError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|source| MetricsError::Http { source })
}

/// Scrapes the endpoint once and returns one sample.
///
/// `start` defines the origin of `elapsed_ns`. The timestamp is recorded
/// immediately before the request is issued, so `elapsed_ns` reflects
/// when the scrape began rather than when the response arrived — keeping
/// it comparable with the other samplers, whose timestamps also precede
/// their reads.
///
/// Failure modes (see [`MetricsError`]): the request itself failing
/// (`Http`), a non-success status (`Status`), or a body that does not
/// contain both KV-cache series for the configured model (`Parse`).
pub async fn scrape_once(
    client: &reqwest::Client,
    config: &MetricsConfig,
    start: Instant,
) -> Result<KvCacheSample, MetricsError> {
    let elapsed_ns = start.elapsed().as_nanos() as u64;

    let response = client
        .get(&config.endpoint)
        .send()
        .await
        .map_err(|source| MetricsError::Http { source })?;

    let status = response.status();
    if !status.is_success() {
        return Err(MetricsError::Status {
            status: status.as_u16(),
        });
    }

    let body = response
        .text()
        .await
        .map_err(|source| MetricsError::Http { source })?;

    let (hits, queries) = parse_kvcache(&body, &config.model_name, config.engine)?;

    // An absent numerator is not a zero one (ADR-014 D4). `KvCacheSample`
    // cannot hold the distinction, so rather than write absence as zero
    // this scrape produces nothing at all — the same outcome the loop
    // already gives any failed tick, and the timeline carries on.
    let hits = hits.ok_or_else(|| MetricsError::Parse {
        detail: format!(
            "no hit-rate numerator for model_name {:?}: the body carries the \
             denominator but nothing to count against it",
            config.model_name
        ),
    })?;

    Ok(KvCacheSample {
        elapsed_ns,
        hits,
        queries,
    })
}

/// Runs the scrape loop until cancelled.
///
/// Scrapes `config.endpoint` every `config.sample_period`, accumulating
/// each successful sample into a [`KvCacheTimeline`]. The loop terminates
/// when `cancel` is signalled (the orchestrator drops the
/// [`oneshot::Sender`] or sends a unit value), and returns whatever was
/// collected.
///
/// If the client cannot be built the loop returns an empty timeline
/// rather than propagating — consistent with the best-effort contract:
/// a run that produces no KV-cache data is degraded, not aborted, and
/// the report simply carries no hit rate.
///
/// Per-tick scrape errors are swallowed (a transient endpoint failure,
/// the engine not ready yet, a momentary parse miss). `elapsed_ns`
/// ordering is preserved because the timestamp is taken at the top of
/// each scrape, in tick order.
pub async fn scrape_during(
    config: MetricsConfig,
    start: Instant,
    mut cancel: oneshot::Receiver<()>,
) -> KvCacheTimeline {
    let mut timeline = KvCacheTimeline::new(
        config.sample_period.as_nanos() as u64,
        config.engine.accounting(),
    );

    let client = match build_client() {
        Ok(c) => c,
        Err(_) => return timeline,
    };

    let mut ticker = interval(config.sample_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            // Cancellation wins over the tick, same as sampler.rs.
            _ = &mut cancel => break,

            _ = ticker.tick() => {
                match scrape_once(&client, &config, start).await {
                    Ok(sample) => timeline.push(sample),
                    Err(_) => {
                        // Best-effort: swallow per-tick errors so a
                        // transient failure does not abort the timeline.
                    }
                }
            }
        }
    }

    timeline
}

/// Scrapes the endpoint once and returns one phase sample.
///
/// The phase twin of [`scrape_once`]. Same timestamp discipline: the
/// `elapsed_ns` origin is taken immediately before the request is
/// issued, keeping it comparable with every other sampler. Same failure
/// modes — `Http` on a request error, `Status` on a non-success code,
/// `Parse` when the body lacks any of the four phase series for the
/// configured model.
pub async fn scrape_phase_once(
    client: &reqwest::Client,
    config: &MetricsConfig,
    start: Instant,
) -> Result<PhaseSample, MetricsError> {
    let elapsed_ns = start.elapsed().as_nanos() as u64;

    let response = client
        .get(&config.endpoint)
        .send()
        .await
        .map_err(|source| MetricsError::Http { source })?;

    let status = response.status();
    if !status.is_success() {
        return Err(MetricsError::Status {
            status: status.as_u16(),
        });
    }

    let body = response
        .text()
        .await
        .map_err(|source| MetricsError::Http { source })?;

    let (prompt_tokens, generation_tokens, prefill_ns, decode_ns) =
        parse_phase(&body, &config.model_name, config.engine)?;

    Ok(PhaseSample {
        elapsed_ns,
        prompt_tokens,
        generation_tokens,
        prefill_ns,
        decode_ns,
    })
}

/// Runs the phase scrape loop until cancelled.
///
/// The phase twin of [`scrape_during`], and deliberately a separate loop
/// rather than folded into the KV scrape (ADR-012): the two derivations
/// are independent first/last reductions over the same run window, so
/// they share `start` and the cancel signal — the shared clock that the
/// positioning rests on — but not a single GET. Keeping them separate
/// leaves the green ADR-011 KV path untouched; the orchestrator spawns
/// this as a second best-effort task and folds `PhaseTimeline` at the
/// same stage as the KV timeline.
///
/// Same contract as the KV loop: `MissedTickBehavior::Skip`, a `biased`
/// cancel arm that wins over the tick, per-tick errors swallowed, and an
/// empty timeline (never a propagated error) if the client cannot be
/// built.
pub async fn scrape_phase_during(
    config: MetricsConfig,
    start: Instant,
    mut cancel: oneshot::Receiver<()>,
) -> PhaseTimeline {
    let mut timeline = PhaseTimeline::new(config.sample_period.as_nanos() as u64);

    let client = match build_client() {
        Ok(c) => c,
        Err(_) => return timeline,
    };

    let mut ticker = interval(config.sample_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            _ = &mut cancel => break,

            _ = ticker.tick() => {
                match scrape_phase_once(&client, &config, start).await {
                    Ok(sample) => timeline.push(sample),
                    Err(_) => {
                        // Best-effort: swallow per-tick errors, same as
                        // the KV loop.
                    }
                }
            }
        }
    }

    timeline
}

/// Scrapes the endpoint once and returns one speculative reading.
///
/// Unlike [`scrape_once`] and [`scrape_phase_once`], this returns the raw
/// reading rather than a sample. `scrape_once` turns an absent hit-rate
/// numerator into an error because `KvCacheSample` cannot express absence;
/// `SpecSample` cannot either, but the decision was already placed in
/// `is-core`, where [`is_core::SpecTimeline::push`] takes three `Option`s and
/// drops the tick if any is missing. Returning a `Result<SpecSample, _>` here
/// would move that decision into the I/O layer and leave `push`'s
/// partial-family branch unreachable from the only path that calls it
/// (ADR-016 D3).
///
/// So the three counters travel out exactly as the body carried them, paired
/// with the elapsed timestamp. Same timestamp discipline as the other two:
/// taken immediately before the request is issued, so it reflects when the
/// scrape began.
///
/// `Err` keeps its usual meaning — `Http` on a request failure, `Status` on a
/// non-success code, `Parse` on a line that exists and does not parse. A body
/// from an engine that is not speculating is none of those: it yields
/// `(elapsed, None, None, None)`.
pub async fn scrape_spec_once(
    client: &reqwest::Client,
    config: &MetricsConfig,
    start: Instant,
) -> Result<(u64, SpecReading), MetricsError> {
    let elapsed_ns = start.elapsed().as_nanos() as u64;

    let response = client
        .get(&config.endpoint)
        .send()
        .await
        .map_err(|source| MetricsError::Http { source })?;

    let status = response.status();
    if !status.is_success() {
        return Err(MetricsError::Status {
            status: status.as_u16(),
        });
    }

    let body = response
        .text()
        .await
        .map_err(|source| MetricsError::Http { source })?;

    let reading = parse_spec(&body, &config.model_name, config.engine)?;

    Ok((elapsed_ns, reading))
}

/// Runs the speculative scrape loop until cancelled.
///
/// The third loop over the same run window, separate from the KV loop
/// (ADR-011) and the phase loop (ADR-012) for the reason ADR-012 established
/// and ADR-016 D2 restates: these are independent first/last reductions, so
/// they share `start` and the cancel discipline — the clock the correlation
/// with energy rests on — and nothing else.
///
/// Same contract as the other two, without exception:
/// `MissedTickBehavior::Skip`, a `biased` cancel arm that wins over the tick,
/// per-tick errors swallowed, and an empty timeline rather than a propagated
/// error if the client cannot be built.
///
/// An engine that is not speculating produces an empty timeline through a
/// different route than an engine that fails to answer: every tick succeeds
/// and every tick is dropped by `push`. Both are empty, and the report says
/// only that no speculative measurement exists — which is all either fact
/// supports.
pub async fn scrape_spec_during(
    config: MetricsConfig,
    start: Instant,
    mut cancel: oneshot::Receiver<()>,
) -> SpecTimeline {
    let mut timeline = SpecTimeline::new(config.sample_period.as_nanos() as u64);

    let client = match build_client() {
        Ok(c) => c,
        Err(_) => return timeline,
    };

    let mut ticker = interval(config.sample_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            _ = &mut cancel => break,

            _ = ticker.tick() => {
                if let Ok((elapsed_ns, (draft_tokens, accepted_tokens, drafts))) =
                    scrape_spec_once(&client, &config, start).await
                {
                    // The return value is discarded for the same reason
                    // per-tick errors are: a dropped tick is a normal
                    // outcome of best-effort sampling, and whether this
                    // one was kept is not a fact the loop acts on.
                    timeline.push(elapsed_ns, draft_tokens, accepted_tokens, drafts);
                }
            }
        }
    }

    timeline
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Engine;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A body in the shape a real vLLM endpoint emits, generated with
    /// `prometheus_client` — the library vLLM registers its counters
    /// with. Scrape mechanics are exercised against this rather than
    /// against the `llm-d-inference-sim` body, which does not speak the
    /// same vocabulary for every series — see
    /// `parse::tests::the_simulator_is_not_a_vllm_endpoint_for_kv`.
    const FIXTURE: &str = include_str!("../tests/fixtures/vllm-prometheus-client-exposition.txt");

    #[tokio::test]
    async fn scrape_once_parses_the_fixture_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let sample = scrape_once(&client, &config, Instant::now()).await.unwrap();

        assert_eq!(sample.hits, 144);
        assert_eq!(sample.queries, 270);
    }

    #[tokio::test]
    async fn scrape_once_surfaces_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let err = scrape_once(&client, &config, Instant::now())
            .await
            .unwrap_err();

        assert!(matches!(err, MetricsError::Status { status: 503 }));
    }

    #[tokio::test]
    async fn scrape_once_reports_parse_failure_on_missing_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string("# nothing useful here\n"))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let err = scrape_once(&client, &config, Instant::now())
            .await
            .unwrap_err();

        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[tokio::test]
    async fn scrape_during_returns_empty_timeline_if_cancelled_immediately() {
        let (tx, rx) = oneshot::channel();
        let config = MetricsConfig::new("http://127.0.0.1:1/metrics", "m", Engine::Vllm);
        // Cancel before the first tick fires.
        tx.send(()).unwrap();
        let timeline = scrape_during(config, Instant::now(), rx).await;
        assert!(timeline.is_empty());
    }

    #[tokio::test]
    async fn scrape_during_collects_samples_until_cancelled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::with_period(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
            std::time::Duration::from_millis(20),
        );
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(scrape_during(config, Instant::now(), rx));

        // Let a few ticks fire, then cancel.
        tokio::time::sleep(std::time::Duration::from_millis(90)).await;
        tx.send(()).unwrap();
        let timeline = handle.await.unwrap();

        assert!(
            !timeline.is_empty(),
            "expected at least one sample over ~90ms at 20ms cadence"
        );
        // Every collected sample must carry the fixture's counter values.
        for s in &timeline.samples {
            assert_eq!(s.hits, 144);
            assert_eq!(s.queries, 270);
        }
    }

    #[tokio::test]
    async fn scrape_phase_once_parses_the_fixture_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let sample = scrape_phase_once(&client, &config, Instant::now())
            .await
            .unwrap();

        assert_eq!(sample.prompt_tokens, 196);
        assert_eq!(sample.generation_tokens, 38);
        assert_eq!(sample.prefill_ns, Some(14493));
        assert_eq!(sample.decode_ns, Some(28432));
    }

    #[tokio::test]
    async fn scrape_phase_once_surfaces_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let err = scrape_phase_once(&client, &config, Instant::now())
            .await
            .unwrap_err();

        assert!(matches!(err, MetricsError::Status { status: 503 }));
    }

    #[tokio::test]
    async fn scrape_phase_during_returns_empty_timeline_if_cancelled_immediately() {
        let (tx, rx) = oneshot::channel();
        let config = MetricsConfig::new("http://127.0.0.1:1/metrics", "m", Engine::Vllm);
        tx.send(()).unwrap();
        let timeline = scrape_phase_during(config, Instant::now(), rx).await;
        assert!(timeline.is_empty());
    }

    #[tokio::test]
    async fn scrape_phase_during_collects_samples_until_cancelled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::with_period(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
            std::time::Duration::from_millis(20),
        );
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(scrape_phase_during(config, Instant::now(), rx));

        tokio::time::sleep(std::time::Duration::from_millis(90)).await;
        tx.send(()).unwrap();
        let timeline = handle.await.unwrap();

        assert!(
            !timeline.is_empty(),
            "expected at least one sample over ~90ms at 20ms cadence"
        );
        for s in &timeline.samples {
            assert_eq!(s.prompt_tokens, 196);
            assert_eq!(s.generation_tokens, 38);
            assert_eq!(s.prefill_ns, Some(14493));
            assert_eq!(s.decode_ns, Some(28432));
        }
    }

    /// A body from a server started with a speculative config. The
    /// non-speculative FIXTURE above is kept as the other case: both are
    /// real vLLM shapes, and the difference between them is a run flag.
    const SPEC_FIXTURE: &str = include_str!("../tests/fixtures/vllm-spec-decode-exposition.txt");

    #[tokio::test]
    async fn scrape_spec_once_reads_the_three_counters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SPEC_FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let (_, (draft, accepted, drafts)) = scrape_spec_once(&client, &config, Instant::now())
            .await
            .unwrap();

        assert_eq!(draft, Some(640));
        assert_eq!(accepted, Some(384));
        assert_eq!(drafts, Some(128));
    }

    #[tokio::test]
    async fn scrape_spec_once_on_a_non_speculating_body_is_not_an_error() {
        // ADR-016 D1 at the I/O boundary: a server without a speculative
        // config answers 200 with a body carrying none of the three, and
        // that is a successful scrape of a run that did not speculate.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let (_, reading) = scrape_spec_once(&client, &config, Instant::now())
            .await
            .unwrap();

        assert_eq!(reading, (None, None, None));
    }

    #[tokio::test]
    async fn scrape_spec_once_surfaces_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let config = MetricsConfig::new(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
        );
        let client = build_client().unwrap();
        let err = scrape_spec_once(&client, &config, Instant::now())
            .await
            .unwrap_err();

        assert!(matches!(err, MetricsError::Status { status: 503 }));
    }

    #[tokio::test]
    async fn scrape_spec_during_returns_empty_timeline_if_cancelled_immediately() {
        let (tx, rx) = oneshot::channel();
        let config = MetricsConfig::new("http://127.0.0.1:1/metrics", "m", Engine::Vllm);
        tx.send(()).unwrap();
        let timeline = scrape_spec_during(config, Instant::now(), rx).await;
        assert!(timeline.is_empty());
    }

    #[tokio::test]
    async fn scrape_spec_during_collects_samples_until_cancelled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SPEC_FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::with_period(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
            std::time::Duration::from_millis(20),
        );
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(scrape_spec_during(config, Instant::now(), rx));

        tokio::time::sleep(std::time::Duration::from_millis(90)).await;
        tx.send(()).unwrap();
        let timeline = handle.await.unwrap();

        assert!(
            !timeline.is_empty(),
            "expected at least one sample over ~90ms at 20ms cadence"
        );
        for s in &timeline.samples {
            assert_eq!(s.draft_tokens, 640);
            assert_eq!(s.accepted_tokens, 384);
            assert_eq!(s.drafts, 128);
        }
    }

    #[tokio::test]
    async fn a_non_speculating_engine_yields_an_empty_timeline_through_push() {
        // The route that distinguishes this loop from the other two: every
        // tick succeeds, every tick is dropped, and the timeline is empty
        // without a single error having occurred. An engine that fails to
        // answer produces the same empty timeline by the other route, and
        // the report does not claim to tell them apart (ADR-016 D2).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;

        let config = MetricsConfig::with_period(
            format!("{}/metrics", server.uri()),
            "Qwen/Qwen2.5-7B-Instruct",
            Engine::Vllm,
            std::time::Duration::from_millis(20),
        );
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(scrape_spec_during(config, Instant::now(), rx));

        tokio::time::sleep(std::time::Duration::from_millis(90)).await;
        tx.send(()).unwrap();
        let timeline = handle.await.unwrap();

        assert!(timeline.is_empty());
    }
}
