use deser_value::Value;

use super::model::IndexEntry;
use super::names;
use super::{input, integrity, Rejection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Native,
    Upstream,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeRecord {
    pub entry: IndexEntry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamRecord {
    pub name: String,
    pub vers: String,
    pub checksum: String,
    pub raw_index_line: String,
    pub acquired_at: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Native(NativeRecord),
    Upstream(UpstreamRecord),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdoptedLine {
    pub record: Record,
    pub checksum: String,
}

pub fn reconstruct_index(
    view: View,
    relative: &str,
    text: &str,
) -> Result<Vec<AdoptedLine>, Rejection> {
    let mut records = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        records.push(reconstruct_line(view, relative, line)?);
    }
    Ok(records)
}

pub fn reconstruct_line(
    view: View,
    relative: &str,
    line: &str,
) -> Result<AdoptedLine, Rejection> {
    let value: Value = deser_json::from_str(line).map_err(|error| input(error.to_string()))?;
    let entry = IndexEntry::from_value(&value)?;
    names::validate_crate_name(&entry.name)?;
    let expected = names::shard_for_name(&names::lower_name(&entry.name)).ok_or_else(|| {
        integrity(format!("index line name is not ASCII: {}", entry.name))
    })?;
    if expected != relative {
        return Err(integrity(format!(
            "index line for {} is stored at {relative}",
            entry.name
        )));
    }
    let checksum = entry.cksum.clone();
    let record = match view {
        View::Native => Record::Native(NativeRecord { entry }),
        View::Upstream => Record::Upstream(UpstreamRecord {
            name: entry.name.clone(),
            vers: entry.vers.clone(),
            checksum: entry.cksum.clone(),
            raw_index_line: line.trim().to_string(),
            acquired_at: 0,
        }),
    };
    Ok(AdoptedLine { record, checksum })
}

#[cfg(test)]
mod tests {
    use super::{Record, View, reconstruct_index, reconstruct_line};

    const LINE: &str = r#"{"name":"abcd","vers":"1.0.0","deps":[],"cksum":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","features":{},"yanked":false}"#;

    #[test]
    fn native_lines_reconstruct_into_index_entries() {
        let adopted = reconstruct_line(View::Native, "ab/cd/abcd", LINE).expect("native line");
        match adopted.record {
            Record::Native(record) => assert_eq!(record.entry.vers, "1.0.0"),
            Record::Upstream(_) => panic!("expected a native record"),
        }
        assert_eq!(
            adopted.checksum,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn upstream_records_keep_the_trimmed_raw_line() {
        let text = format!("{LINE}\r\n\n");
        let records = reconstruct_index(View::Upstream, "ab/cd/abcd", &text).expect("index text");
        assert_eq!(records.len(), 1);
        match &records[0].record {
            Record::Upstream(record) => {
                assert_eq!(record.name, "abcd");
                assert_eq!(record.raw_index_line, LINE);
                assert_eq!(record.acquired_at, 0);
            }
            Record::Native(_) => panic!("expected an upstream record"),
        }
    }

    #[test]
    fn rejects_lines_stored_at_the_wrong_shard() {
        let error = reconstruct_line(View::Native, "ab/ce/abcd", LINE).expect_err("wrong shard");
        assert_eq!(
            error.detail,
            "integrity: index line for abcd is stored at ab/ce/abcd"
        );
    }

    #[test]
    fn rejects_names_outside_the_crate_name_rules() {
        let line = LINE.replace("\"abcd\"", "\"café\"");
        let error = reconstruct_line(View::Native, "ca/fé/café", &line).expect_err("bad name");
        assert_eq!(error.detail, "input: invalid crate name character");
    }
}
