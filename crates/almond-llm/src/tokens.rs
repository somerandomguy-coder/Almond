//! Cheap, tokenizer-free token estimates used for `num_ctx` and map-reduce budgeting.
//!
//! Calibrated against Ollama's `prompt_eval_count` for qwen3.5 / gemma4 on the eval set: English
//! averages ~4 ASCII chars per token; Vietnamese syllables with diacritics cost noticeably more
//! (non-ASCII letters often split into their own byte-level tokens). We over-estimate on purpose
//! (×1.15) — running out of context silently truncates the prompt in Ollama.

/// Estimated token count for `s` (never 0 for non-empty input).
pub fn estimate_tokens(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    let mut ascii = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
    }
    let est = ascii as f64 / 3.6 + other as f64 * 0.75;
    ((est * 1.15).ceil() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rough_magnitudes() {
        assert_eq!(estimate_tokens(""), 0);
        let en = "We should hire two backend engineers by December.";
        let n = estimate_tokens(en);
        assert!((10..=20).contains(&n), "{n}");
        let vi = "Ngân sách marketing là 200 triệu đồng.";
        let m = estimate_tokens(vi);
        assert!((10..=20).contains(&m), "{m}");
    }
}
