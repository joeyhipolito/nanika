use sha2::{Digest, Sha256};

/// Generate a deterministic tracker ID from title and timestamp.
/// Format: trk-XXXX where XXXX is the first 4 hex chars of SHA256(title + timestamp).
pub fn generate(title: &str, timestamp: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update(b":");
    hasher.update(timestamp.as_bytes());
    let result = hasher.finalize();
    let hex = hex::encode(&result[..2]); // 2 bytes = 4 hex chars
    format!("trk-{}", hex.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_format() {
        let id = generate("test issue", "2026-03-26T10:00:00Z");
        assert!(id.starts_with("trk-"));
        assert_eq!(id.len(), 8); // "trk-" (4) + 4 hex chars (2 bytes)
    }

    #[test]
    fn test_generate_deterministic() {
        let id1 = generate("same title", "2026-03-26T10:00:00Z");
        let id2 = generate("same title", "2026-03-26T10:00:00Z");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_generate_differs_by_title() {
        let id1 = generate("title a", "2026-03-26T10:00:00Z");
        let id2 = generate("title b", "2026-03-26T10:00:00Z");
        assert_ne!(id1, id2);
    }
}
