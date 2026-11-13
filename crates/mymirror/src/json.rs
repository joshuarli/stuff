use deser_value::{Kind, Map, Seq, Value};

pub(crate) fn sort_canonical_value(value: &mut Value) {
    if let Some(seq) = value.as_seq_mut() {
        for item in seq.iter_mut() {
            sort_canonical_value(item);
        }
        return;
    }
    if let Some(map) = value.as_map_mut() {
        for index in 0..map.len() {
            if let Some((_, item)) = map.get_index_mut(index) {
                sort_canonical_value(item);
            }
        }
        map.sort_by(|left, _, right, _| {
            let left = left.as_str().map(str::to_string).unwrap_or_else(|| format!("{left:?}"));
            let right = right.as_str().map(str::to_string).unwrap_or_else(|| format!("{right:?}"));
            left.cmp(&right)
        });
    }
}

pub(crate) fn canonical_string(value: &Value) -> Result<String, crate::error::Error> {
    let mut sorted = value.clone();
    sort_canonical_value(&mut sorted);
    deser_json::to_string(&sorted)
        .map_err(|error| crate::error::Error::operation(format!("value is not JSON serializable: {error}")))
}

pub(crate) fn render_pretty(value: &Value) -> Result<String, crate::error::Error> {
    let mut sorted = value.clone();
    sort_canonical_value(&mut sorted);
    let mut out = String::new();
    write_value(&sorted, &mut out, Some(2), 0)?;
    out.push('\n');
    Ok(out)
}

fn write_value(
    value: &Value,
    out: &mut String,
    indent: Option<usize>,
    level: usize,
) -> Result<(), crate::error::Error> {
    let newline = |out: &mut String, level: usize| {
        if let Some(width) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(width * level));
        }
    };
    match &**value {
        Kind::Null => out.push_str("null"),
        Kind::Bool(true) => out.push_str("true"),
        Kind::Bool(false) => out.push_str("false"),
        Kind::U64(n) => out.push_str(&n.to_string()),
        Kind::I64(n) => out.push_str(&n.to_string()),
        Kind::F64(f) => out.push_str(&format!("{f:?}")),
        Kind::Str(_) | Kind::Lexical(_) => out.push_str(&json_string(value)?),
        _ if value.as_seq().is_some() => {
            let seq = value.as_seq().expect("checked as seq");
            out.push('[');
            for (index, item) in seq.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write_value(item, out, indent, level + 1)?;
            }
            if !seq.is_empty() {
                newline(out, level);
            }
            out.push(']');
        }
        _ => {
            let map = value.as_map().expect("checked as map");
            out.push('{');
            let mut first = true;
            for index in 0..map.len() {
                let Some((key, item)) = map.get_index(index) else {
                    continue;
                };
                if !first {
                    out.push(',');
                }
                first = false;
                newline(out, level + 1);
                out.push_str(&json_string(key)?);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(item, out, indent, level + 1)?;
            }
            if !first {
                newline(out, level);
            }
            out.push('}');
        }
    }
    Ok(())
}

fn json_string(value: &Value) -> Result<String, crate::error::Error> {
    deser_json::to_string(value)
        .map_err(|error| crate::error::Error::operation(format!("value is not JSON serializable: {error}")))
}

pub(crate) fn string_map(entries: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::from(map)
}

pub(crate) fn seq(items: Vec<Value>) -> Value {
    let mut seq = Seq::new();
    for item in items {
        seq.push(item);
    }
    Value::from(seq)
}
