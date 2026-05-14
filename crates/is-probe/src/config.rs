//! Configuration for a single probe run.
//!
//! `ProbeConfig` describes what the probe should ask the engine to
//! do: which endpoint, which model, what prompt, and the request
//! parameters that affect generation. It carries no logic — it is
//! the input to the probe, kept separate from the streaming code so
//! that parameter handling never mixes with measurement.

/// The configuration for one probe run against an inference engine.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Base URL of the engine's OpenAI-compatible API, without a
    /// trailing path. The probe appends the chat completions path
    /// itself. Example: `http://localhost:8080`.
    pub endpoint: String,

    /// The model identifier to request, as the engine expects it in
    /// the `model` field of the request body.
    pub model: String,

    /// The prompt sent as the single user message of the request.
    pub prompt: String,

    /// The maximum number of tokens the engine should generate.
    /// Bounds the length of a probe run.
    pub max_tokens: u32,

    /// Sampling temperature. Profiling runs default to `0.0` for
    /// determinism, so timing is not perturbed by sampling
    /// variation between runs.
    pub temperature: f32,
}

impl ProbeConfig {
    /// Creates a probe configuration with the profiling defaults:
    /// temperature `0.0` for deterministic, repeatable runs.
    pub fn new(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        prompt: impl Into<String>,
        max_tokens: u32,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            prompt: prompt.into(),
            max_tokens,
            temperature: 0.0,
        }
    }

    /// Returns the full chat completions URL for this config,
    /// joining the base endpoint with the OpenAI-compatible path.
    /// Any trailing slash on the endpoint is handled so the result
    /// never contains a doubled slash.
    pub fn completions_url(&self) -> String {
        let base = self.endpoint.trim_end_matches('/');
        format!("{base}/v1/chat/completions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_applies_profiling_defaults() {
        let cfg = ProbeConfig::new("http://localhost:8080", "llama3", "hi", 64);
        assert_eq!(cfg.endpoint, "http://localhost:8080");
        assert_eq!(cfg.model, "llama3");
        assert_eq!(cfg.prompt, "hi");
        assert_eq!(cfg.max_tokens, 64);
        assert_eq!(cfg.temperature, 0.0);
    }

    #[test]
    fn completions_url_appends_the_openai_path() {
        let cfg = ProbeConfig::new("http://localhost:8080", "llama3", "hi", 64);
        assert_eq!(
            cfg.completions_url(),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[test]
    fn completions_url_handles_a_trailing_slash() {
        let cfg = ProbeConfig::new("http://localhost:8080/", "llama3", "hi", 64);
        assert_eq!(
            cfg.completions_url(),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[test]
    fn completions_url_handles_multiple_trailing_slashes() {
        let cfg = ProbeConfig::new("http://localhost:8080///", "llama3", "hi", 64);
        assert_eq!(
            cfg.completions_url(),
            "http://localhost:8080/v1/chat/completions"
        );
    }
}
