use deser_value::{Kind, Value};

pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

pub fn compact(value: &Value) -> String {
    match value.kind() {
        Kind::Null => "null".to_string(),
        Kind::Bool(true) => "true".to_string(),
        Kind::Bool(false) => "false".to_string(),
        Kind::U64(number) => number.to_string(),
        Kind::I64(number) => number.to_string(),
        Kind::F32(number) => float_json(f64::from(*number)),
        Kind::F64(number) => float_json(*number),
        Kind::Char(character) => json_string(&character.to_string()),
        Kind::Str(text) | Kind::Lexical(text) => json_string(text),
        Kind::Seq(seq) => {
            let items: Vec<String> = seq.iter().map(compact).collect();
            format!("[{}]", items.join(","))
        }
        Kind::Map(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        json_string(key.as_str().unwrap_or_default()),
                        compact(value)
                    )
                })
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        _ => "null".to_string(),
    }
}

pub fn error_body(detail: &str) -> String {
    format!("{{\"errors\":[{{\"detail\":{}}}]}}", json_string(detail))
}

fn float_json(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_string();
    }
    let text = format!("{value:?}");
    let Some((mantissa, exponent)) = text.split_once('e') else {
        return text;
    };
    let (sign, digits) = match exponent.strip_prefix('-') {
        Some(digits) => ('-', digits),
        None => ('+', exponent.strip_prefix('+').unwrap_or(exponent)),
    };
    if digits.len() >= 2 {
        format!("{mantissa}e{sign}{digits}")
    } else {
        format!("{mantissa}e{sign}0{digits}")
    }
}

#[cfg(test)]
mod tests {
    use super::json_string;

    #[test]
    fn escaping_matches_the_index_contract() {
        assert_eq!(
            json_string("a\"b\\c\n\r\t"),
            "\"a\\\"b\\\\c\\n\\r\\t\""
        );
        assert_eq!(json_string("\u{1}\u{1f}"), "\"\\u0001\\u001f\"");
        assert_eq!(json_string("café/日本"), "\"café/日本\"");
    }
}
