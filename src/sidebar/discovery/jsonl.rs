use std::io::{BufRead, Read, Seek, SeekFrom};

const MAX_CONTEXT_TOKENS: f64 = 200_000.0;

#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    pub topic: String,
    pub git_branch: String,
    pub context_pct: u8,
}

pub fn extract_session_meta(jsonl_path: &str, max_topic_chars: usize) -> SessionMeta {
    let mut meta = SessionMeta::default();
    let path = std::path::Path::new(jsonl_path);

    if !path.exists() {
        return meta;
    }

    // Forward pass: first user message (topic) + git branch
    if let Ok(file) = std::fs::File::open(path) {
        let reader = std::io::BufReader::new(file);
        let mut topic_found = false;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };

            let obj: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if meta.git_branch.is_empty() {
                if let Some(branch) = obj.get("gitBranch").and_then(|v| v.as_str()) {
                    meta.git_branch = branch.to_string();
                }
            }

            if !topic_found
                && obj.get("type").and_then(|v| v.as_str()) == Some("user")
            {
                if let Some(text) =
                    extract_text_from_content(obj.get("message").and_then(|m| m.get("content")))
                {
                    if !text.is_empty() {
                        meta.topic = text.chars().take(max_topic_chars).collect();
                        topic_found = true;
                    }
                }
            }

            if topic_found && !meta.git_branch.is_empty() {
                break;
            }
        }
    }

    // Reverse pass: last 50KB for context %
    if let Ok(mut file) = std::fs::File::open(path) {
        let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let read_from = file_size.saturating_sub(50_000);

        if read_from > 0 {
            let _ = file.seek(SeekFrom::Start(read_from));
        }

        let mut raw = Vec::new();
        let _ = file.read_to_end(&mut raw);
        let tail = String::from_utf8_lossy(&raw);

        let mut lines = tail.lines();
        if read_from > 0 {
            // Skip potentially partial first line
            let _ = lines.next();
        }

        let mut last_usage: Option<serde_json::Value> = None;

        for line in lines {
            let obj: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if obj.get("type").and_then(|v| v.as_str()) == Some("assistant") {
                if let Some(usage) = obj.get("message").and_then(|m| m.get("usage")) {
                    last_usage = Some(usage.clone());
                }
            }
        }

        if let Some(usage) = last_usage {
            let input = usage
                .get("input_tokens")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let cache_create = usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let output = usage
                .get("output_tokens")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);

            let total = input + cache_read + cache_create + output;
            let pct = (total / MAX_CONTEXT_TOKENS * 100.0).round().min(100.0) as u8;
            meta.context_pct = pct;
        }
    }

    meta
}

fn extract_text_from_content(content: Option<&serde_json::Value>) -> Option<String> {
    let content = content?;

    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    } else if let Some(s) = content.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_path(name: &str) -> String {
        format!("/tmp/cockpit_test_{name}_{}.jsonl", std::process::id())
    }

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
    }

    fn user_message_line(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "content": [{"type": "text", "text": text}]
            }
        })
        .to_string()
    }

    fn assistant_message_line(input_tokens: u64, cache_read: u64, cache_create: u64, output_tokens: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "usage": {
                    "input_tokens": input_tokens,
                    "cache_read_input_tokens": cache_read,
                    "cache_creation_input_tokens": cache_create,
                    "output_tokens": output_tokens
                }
            }
        })
        .to_string()
    }

    #[test]
    fn test_extract_meta_basic() {
        let path = test_path("basic");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{}", user_message_line("Hello world")).unwrap();
            // 10000 + 20000 + 30000 + 5000 = 65000 => 65000/200000*100 = 32.5 => 33
            writeln!(f, "{}", assistant_message_line(10_000, 20_000, 30_000, 5_000)).unwrap();
        }
        let meta = extract_session_meta(&path, 100);
        cleanup(&path);

        assert_eq!(meta.topic, "Hello world");
        assert_eq!(meta.context_pct, 33); // (65000/200000)*100 rounded
    }

    #[test]
    fn test_extract_meta_empty_file() {
        let path = test_path("empty");
        {
            let _ = std::fs::File::create(&path).unwrap();
        }
        let meta = extract_session_meta(&path, 100);
        cleanup(&path);

        assert_eq!(meta.topic, "");
        assert_eq!(meta.context_pct, 0);
        assert_eq!(meta.git_branch, "");
    }

    #[test]
    fn test_extract_meta_with_multibyte_utf8() {
        let path = test_path("multibyte");
        {
            let mut f = std::fs::File::create(&path).unwrap();

            // Write user message first
            writeln!(f, "{}", user_message_line("Multibyte test")).unwrap();

            // Fill with lines containing multi-byte emoji to push size > 50KB.
            // Each fire emoji is 4 bytes. We need enough data so the 50KB-from-EOF
            // seek lands in the middle of an emoji.
            // Write many lines of emoji to create a ~60KB padding block.
            let emoji_line = "🔥".repeat(500); // 500 * 4 = 2000 bytes per line
            for _ in 0..35 {
                // 35 * ~2000 = ~70KB of emoji lines (valid JSON strings)
                writeln!(
                    f,
                    "{}",
                    serde_json::json!({"type": "filler", "data": emoji_line}).to_string()
                )
                .unwrap();
            }

            // Write the assistant message with usage at the very end
            // 50000 + 50000 + 50000 + 10000 = 160000 => 160000/200000*100 = 80
            writeln!(f, "{}", assistant_message_line(50_000, 50_000, 50_000, 10_000)).unwrap();
        }

        let meta = extract_session_meta(&path, 100);
        cleanup(&path);

        assert_eq!(meta.topic, "Multibyte test");
        // The key assertion: context_pct must be extracted even when the 50KB
        // seek lands mid-emoji. With the bug, this will be 0.
        assert_eq!(meta.context_pct, 80);
    }
}
