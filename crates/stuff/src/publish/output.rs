#![forbid(unsafe_code)]

use crate::publish::{Outcome, PublishError, Registry};

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn packages(values: &[String]) -> String {
    let entries: Vec<String> = values
        .iter()
        .map(|value| format!("\"{}\"", escape(value)))
        .collect();
    format!("[{}]", entries.join(","))
}

struct Fields<'a> {
    registry: Registry,
    status: &'a str,
    generation: Option<&'a str>,
    packages: &'a [String],
    dry_run: bool,
    uploaded_objects: usize,
    uploaded_bytes: u64,
    skipped_objects: usize,
}

fn object(fields: &Fields<'_>) -> String {
    let generation = match fields.generation {
        Some(value) => format!("\"{}\"", escape(value)),
        None => "null".to_string(),
    };
    format!(
        "{{\"registry\":\"{}\",\"status\":\"{}\",\"generation\":{},\"packages\":{},\"dry_run\":{},\"uploaded_objects\":{},\"uploaded_bytes\":{},\"skipped_objects\":{}}}",
        fields.registry.as_str(),
        fields.status,
        generation,
        packages(fields.packages),
        fields.dry_run,
        fields.uploaded_objects,
        fields.uploaded_bytes,
        fields.skipped_objects,
    )
}

pub fn human(outcome: &Outcome) -> String {
    let dry_run = if outcome.dry_run { " (dry run)" } else { "" };
    format!(
        "{} {}{} generation {} uploaded {} objects ({} bytes), skipped {} objects",
        outcome.registry.as_str(),
        outcome.status.as_str(),
        dry_run,
        outcome.generation,
        outcome.uploaded_objects,
        outcome.uploaded_bytes,
        outcome.skipped_objects,
    )
}

pub fn json(outcome: &Outcome) -> String {
    object(&Fields {
        registry: outcome.registry,
        status: outcome.status.as_str(),
        generation: Some(&outcome.generation),
        packages: &outcome.packages,
        dry_run: outcome.dry_run,
        uploaded_objects: outcome.uploaded_objects,
        uploaded_bytes: outcome.uploaded_bytes,
        skipped_objects: outcome.skipped_objects,
    })
}

pub fn conflict_json(registry: Registry, dry_run: bool) -> String {
    object(&Fields {
        registry,
        status: "conflict",
        generation: None,
        packages: &[],
        dry_run,
        uploaded_objects: 0,
        uploaded_bytes: 0,
        skipped_objects: 0,
    })
}

pub fn error(error: &PublishError) -> String {
    format!("stuff: {}", one_line(&error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::Status;

    fn outcome(status: Status, dry_run: bool) -> Outcome {
        Outcome {
            registry: Registry::Cargo,
            status,
            generation: "0123456789abcdef01234567".to_string(),
            packages: vec!["demo@1.2.3".to_string()],
            dry_run,
            uploaded_objects: 5,
            uploaded_bytes: 1234,
            skipped_objects: 2,
        }
    }

    #[test]
    fn human_output_is_one_line_per_outcome() {
        assert_eq!(
            human(&outcome(Status::Published, false)),
            "cargo published generation 0123456789abcdef01234567 uploaded 5 objects (1234 bytes), skipped 2 objects"
        );
        assert_eq!(
            human(&outcome(Status::Published, true)),
            "cargo published (dry run) generation 0123456789abcdef01234567 uploaded 5 objects (1234 bytes), skipped 2 objects"
        );
        assert_eq!(
            human(&outcome(Status::Unchanged, false)),
            "cargo unchanged generation 0123456789abcdef01234567 uploaded 5 objects (1234 bytes), skipped 2 objects"
        );
        assert!(!human(&outcome(Status::Published, false)).contains('\n'));
    }

    #[test]
    fn json_output_is_a_stable_object() {
        assert_eq!(
            json(&outcome(Status::Published, false)),
            "{\"registry\":\"cargo\",\"status\":\"published\",\"generation\":\"0123456789abcdef01234567\",\"packages\":[\"demo@1.2.3\"],\"dry_run\":false,\"uploaded_objects\":5,\"uploaded_bytes\":1234,\"skipped_objects\":2}"
        );
        assert_eq!(
            json(&outcome(Status::Unchanged, true)),
            "{\"registry\":\"cargo\",\"status\":\"unchanged\",\"generation\":\"0123456789abcdef01234567\",\"packages\":[\"demo@1.2.3\"],\"dry_run\":true,\"uploaded_objects\":5,\"uploaded_bytes\":1234,\"skipped_objects\":2}"
        );
        assert_eq!(
            conflict_json(Registry::Oci, false),
            "{\"registry\":\"oci\",\"status\":\"conflict\",\"generation\":null,\"packages\":[],\"dry_run\":false,\"uploaded_objects\":0,\"uploaded_bytes\":0,\"skipped_objects\":0}"
        );
    }

    #[test]
    fn errors_render_as_a_single_stderr_line() {
        assert_eq!(
            error(&PublishError::Conflict("current\nchanged".to_string())),
            "stuff: conflict: current changed"
        );
        assert_eq!(
            error(&PublishError::Transport("boom".to_string())),
            "stuff: transport: boom"
        );
    }
}
