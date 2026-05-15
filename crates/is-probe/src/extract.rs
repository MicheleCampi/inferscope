//! Token extraction from a parsed chat chunk.
//!
//! After the wire layer has classified an SSE line and deserialized
//! its JSON, the question is: what does this chunk *mean*? It can
//! carry a new piece of generated content, it can signal that
//! generation is finished, or it can be an intermediate chunk that
//! carries neither — some engines emit empty-delta chunks while
//! buffering internally.
//!
//! [`extract_token`] makes that decision. It is pure: no I/O, no
//! timing, no async. The probe's streaming loop calls it once per
//! chunk to decide what to record.

use crate::wire::ChatChunk;

/// What a parsed chunk represents for the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkOutcome {
    /// A new content token. The string is the incremental text the
    /// chunk added; it may be one or more characters depending on
    /// the engine's tokenizer.
    Token(String),

    /// The engine signalled end of generation. The string is the
    /// finish reason as the engine reported it — typically `"stop"`
    /// or `"length"` — kept for diagnostics.
    Finished(String),

    /// The chunk carried neither new content nor a finish reason.
    /// Some engines emit such intermediate chunks; the probe must
    /// not count them as tokens.
    Empty,
}

/// Classifies a parsed chunk into a [`ChunkOutcome`].
///
/// The OpenAI-compatible streaming shape allows multiple choices
/// per chunk, but probe runs are single-request and the engines we
/// target emit exactly one choice. This function reads the first
/// choice and ignores any others. If no choices are present at all
/// the chunk is treated as empty rather than as an error: real
/// engines occasionally emit keepalive-like chunks during slow
/// generation.
///
/// Precedence when both signals are present: a non-empty content
/// delta is reported as a token even if a finish reason is also
/// set. This matches engines that pack the last token together
/// with the finish signal in a single chunk, which would otherwise
/// lose the last token from the timing record.
pub fn extract_token(chunk: &ChatChunk) -> ChunkOutcome {
    let Some(choice) = chunk.choices.first() else {
        return ChunkOutcome::Empty;
    };

    if let Some(content) = &choice.delta.content {
        if !content.is_empty() {
            return ChunkOutcome::Token(content.clone());
        }
    }

    if let Some(reason) = &choice.finish_reason {
        return ChunkOutcome::Finished(reason.clone());
    }

    ChunkOutcome::Empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{ChatChoice, ChatChunk, ChatDelta};

    fn chunk(content: Option<&str>, finish: Option<&str>) -> ChatChunk {
        ChatChunk {
            choices: vec![ChatChoice {
                delta: ChatDelta {
                    content: content.map(String::from),
                },
                finish_reason: finish.map(String::from),
            }],
        }
    }

    #[test]
    fn content_chunk_yields_a_token() {
        let c = chunk(Some("Hello"), None);
        assert_eq!(extract_token(&c), ChunkOutcome::Token("Hello".to_string()));
    }

    #[test]
    fn finish_chunk_without_content_yields_finished() {
        let c = chunk(None, Some("stop"));
        assert_eq!(
            extract_token(&c),
            ChunkOutcome::Finished("stop".to_string())
        );
    }

    #[test]
    fn empty_delta_without_finish_is_empty() {
        let c = chunk(None, None);
        assert_eq!(extract_token(&c), ChunkOutcome::Empty);
    }

    #[test]
    fn empty_string_content_is_empty_not_a_token() {
        // An engine emitting an empty-string delta is not generating
        // a token; the probe should not count it.
        let c = chunk(Some(""), None);
        assert_eq!(extract_token(&c), ChunkOutcome::Empty);
    }

    #[test]
    fn content_takes_precedence_over_finish_reason() {
        // Some engines pack the final token together with the
        // finish reason. Losing that token from the timing record
        // would be silently wrong.
        let c = chunk(Some("!"), Some("stop"));
        assert_eq!(extract_token(&c), ChunkOutcome::Token("!".to_string()));
    }

    #[test]
    fn no_choices_is_empty_not_an_error() {
        // Real engines sometimes emit choice-less chunks as
        // keepalives during slow generation. The probe should
        // tolerate them as empty.
        let c = ChatChunk { choices: vec![] };
        assert_eq!(extract_token(&c), ChunkOutcome::Empty);
    }

    #[test]
    fn only_the_first_choice_is_examined() {
        // Multi-choice chunks are not part of probe usage. The
        // function reads only choice[0].
        let c = ChatChunk {
            choices: vec![
                ChatChoice {
                    delta: ChatDelta {
                        content: Some("first".to_string()),
                    },
                    finish_reason: None,
                },
                ChatChoice {
                    delta: ChatDelta {
                        content: Some("ignored".to_string()),
                    },
                    finish_reason: None,
                },
            ],
        };
        assert_eq!(extract_token(&c), ChunkOutcome::Token("first".to_string()));
    }
}
