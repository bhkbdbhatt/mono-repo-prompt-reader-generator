use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, CoreBPE};

static ENCODER: OnceLock<Option<CoreBPE>> = OnceLock::new();

fn encoder() -> Option<&'static CoreBPE> {
    ENCODER
        .get_or_init(|| cl100k_base().ok())
        .as_ref()
}

pub fn count_tokens(text: &str) -> usize {
    match encoder() {
        Some(enc) => enc.encode_ordinary(text).len(),
        None => estimate_tokens(text),
    }
}

pub fn count_tokens_batch(texts: &[String]) -> Vec<usize> {
    match encoder() {
        Some(enc) => texts.iter().map(|t| enc.encode_ordinary(t).len()).collect(),
        None => texts.iter().map(|t| estimate_tokens(t)).collect(),
    }
}

fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    chars.div_ceil(4).max(words)
}

pub fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    if count_tokens(text) <= max_tokens {
        return text.to_string();
    }
    if max_tokens == 0 {
        return String::new();
    }

    let ratio = max_tokens as f64 / count_tokens(text) as f64;
    let mut end = ((text.chars().count() as f64 * ratio * 1.15) as usize)
        .min(text.len())
        .max(1);
    end = floor_char_boundary(text, end);

    let mut guard = 0;
    while end > 0 && count_tokens(&text[..end]) > max_tokens && guard < 64 {
        let next = (end * 85 / 100).max(1);
        end = floor_char_boundary(text, next);
        guard += 1;
    }
    while end > 0 && count_tokens(&text[..end]) > max_tokens {
        let mut step = 16usize;
        loop {
            let candidate = end.saturating_sub(step);
            let candidate = floor_char_boundary(text, candidate);
            if candidate == end && step == 1 {
                return String::new();
            }
            end = candidate;
            if count_tokens(&text[..end]) <= max_tokens {
                break;
            }
            if step < 1024 {
                step *= 2;
            }
            if end == 0 {
                return String::new();
            }
        }
    }

    while end > 0 {
        match text[..end].chars().next_back() {
            Some(c) if c.is_whitespace() => end -= c.len_utf8(),
            _ => break,
        }
    }
    text[..end].to_string()
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tokens_positively() {
        let text = "const foo = require('bar'); fn main() {}";
        assert!(count_tokens(text) > 5);
    }

    #[test]
    fn truncation_respects_budget() {
        let text = "word ".repeat(5000);
        let truncated = truncate_to_tokens(&text, 100);
        assert!(count_tokens(&truncated) <= 100, "{}", count_tokens(&truncated));
        assert!(truncated.len() < text.len());
    }

    #[test]
    fn truncation_noop_when_short() {
        assert_eq!(truncate_to_tokens("hello", 100), "hello");
    }

    #[test]
    fn multibyte_truncation_does_not_panic() {
        let text = "日本語テキスト ".repeat(500);
        let truncated = truncate_to_tokens(&text, 50);
        assert!(count_tokens(&truncated) <= 50);
    }

    #[test]
    fn empty_input_is_zero_tokens() {
        assert_eq!(count_tokens(""), 0);
    }
}