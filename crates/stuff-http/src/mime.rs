pub fn content_type_for(file_name: &str) -> &'static str {
    let lower = file_name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "json" => "application/json",
        "html" => "text/html; charset=utf-8",
        "toml" => "text/plain; charset=utf-8",
        "crate" => "application/x-tar",
        "tgz" | "whl" | "gz" => "application/octet-stream",
        "metadata" => "text/plain; charset=utf-8",
        _ if !lower.contains('.') => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_types() {
        assert_eq!(content_type_for("config.json"), "application/json");
        assert_eq!(content_type_for("ab/cd/pkg"), "text/plain; charset=utf-8");
        assert_eq!(content_type_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_for("simple/index.html"), "text/html; charset=utf-8");
    }

    #[test]
    fn artifact_types() {
        assert_eq!(content_type_for("deadbeef.crate"), "application/x-tar");
        assert_eq!(content_type_for("pkg-1.2.3.tgz"), "application/octet-stream");
        assert_eq!(content_type_for("pkg-1.0-py3-none-any.whl"), "application/octet-stream");
        assert_eq!(
            content_type_for("pkg-1.0.metadata"),
            "text/plain; charset=utf-8"
        );
    }
}
