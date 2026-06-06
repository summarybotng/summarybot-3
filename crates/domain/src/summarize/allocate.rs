//! Adaptive token allocation (PRD §12.4; ADR-095).
//!
//! Pure planning: given the input size, the model's context window, and how much
//! room to leave for the response, decide whether the input fits in one pass or
//! must be split into chunks, and how many output tokens each pass may use. The
//! actual tokenization estimate is the host's (a tokenizer is I/O-ish); this
//! works in token counts it is handed.

/// A plan for one summarization call-chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// How many passes the input is split into (≥ 1).
    pub chunks: u32,
    /// Input-token budget per chunk (≤ what fits beside the output reserve).
    pub input_tokens_per_chunk: i64,
    /// Output-token budget per chunk.
    pub output_tokens_per_chunk: i64,
}

/// Plan an allocation.
///
/// - `input_tokens`: estimated tokens of all messages to summarize;
/// - `context_tokens`: the model's context window;
/// - `desired_output`: output tokens wanted for the summary;
/// - `prompt_overhead`: tokens reserved for the system/instruction prompt.
///
/// If everything fits in one window, it's a single pass. Otherwise the input is
/// split into the fewest equal chunks that each fit alongside the output reserve
/// and prompt overhead (a map step; the host then reduces the chunk summaries).
pub fn allocate(
    input_tokens: i64,
    context_tokens: i64,
    desired_output: i64,
    prompt_overhead: i64,
) -> Allocation {
    let input_tokens = input_tokens.max(0);
    let desired_output = desired_output.max(1);
    let prompt_overhead = prompt_overhead.max(0);

    // Room for input in one window, after reserving output + prompt overhead.
    let per_window_input = (context_tokens - desired_output - prompt_overhead).max(1);

    if input_tokens <= per_window_input {
        return Allocation {
            chunks: 1,
            input_tokens_per_chunk: input_tokens.max(1),
            output_tokens_per_chunk: desired_output,
        };
    }

    // Fewest equal chunks that each fit the per-window input budget. (Signed
    // `div_ceil` is still unstable, so ceil-divide by hand over positive values.)
    let chunks = ceil_div(input_tokens, per_window_input);
    let input_per_chunk = ceil_div(input_tokens, chunks);
    Allocation {
        chunks: chunks.min(u32::MAX as i64) as u32,
        input_tokens_per_chunk: input_per_chunk,
        // Per-chunk output is smaller — the chunk summaries get reduced into the
        // final one — but never below a floor.
        output_tokens_per_chunk: (desired_output / chunks).max(256),
    }
}

/// Ceiling division for positive `i64` (`a`, `b` ≥ 1).
fn ceil_div(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_in_one_pass() {
        // 10k input, 200k window, 2k output, 1k overhead → single chunk.
        let a = allocate(10_000, 200_000, 2_000, 1_000);
        assert_eq!(a.chunks, 1);
        assert_eq!(a.input_tokens_per_chunk, 10_000);
        assert_eq!(a.output_tokens_per_chunk, 2_000);
    }

    #[test]
    fn splits_when_input_exceeds_window() {
        // 50k input, 20k window, 2k output, 1k overhead → per-window input ~17k
        // → 3 chunks.
        let a = allocate(50_000, 20_000, 2_000, 1_000);
        assert_eq!(a.chunks, 3);
        assert!(a.input_tokens_per_chunk <= 17_000);
        assert!(a.chunks as i64 * a.input_tokens_per_chunk >= 50_000);
    }

    #[test]
    fn per_chunk_output_has_a_floor() {
        let a = allocate(1_000_000, 20_000, 1_000, 1_000);
        assert!(a.chunks > 1);
        assert!(a.output_tokens_per_chunk >= 256);
    }

    #[test]
    fn tiny_input_is_one_chunk_with_at_least_one_token() {
        let a = allocate(0, 200_000, 2_000, 1_000);
        assert_eq!(a.chunks, 1);
        assert_eq!(a.input_tokens_per_chunk, 1);
    }
}
