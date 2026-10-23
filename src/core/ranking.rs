//! Frozen fusion constants (spec §1.6). The `get_context` pagination
//! constants will follow with the MCP server.

/// RRF smoothing (Cormack et al. 2009).
pub const RRF_K: f32 = 60.0;
/// Weight of the lexical channel; the dense channel is 1.
pub const FTS_CHANNEL_WEIGHT: f32 = 1.2;
/// Per-channel over-fetch factor before RRF fusion (`max(limit * 3, 20)`).
pub const CHANNEL_OVERFETCH: usize = 3;
pub const MIN_CHANNEL_CANDIDATES: usize = 20;

/// Default token budget of `get_context` (about 4 characters per token).
pub const DEFAULT_TOKEN_BUDGET: usize = 2000;

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}
