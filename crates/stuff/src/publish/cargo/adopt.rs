use std::fs;
use std::path::{Path, PathBuf};

use deser_value::Value;

use super::model::IndexEntry;
use super::names;
use super::store::Store;
use super::{Rejection, RejectionKind, input, integrity};

const VIEWS: [(View, &str); 2] = [(View::Native, "registry"), (View::Upstream, "crates-io")];

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdoptSummary {
    pub native: usize,
    pub upstream: usize,
    pub blobs: usize,
}

pub fn adopt_site(store: &Store, site_dir: &Path) -> Result<AdoptSummary, Rejection> {
    store.assert_current_format()?;
    let generation = current_generation_dir(site_dir)?;
    let mut summary = AdoptSummary::default();
    store.with_lock(|| {
        for (view, directory) in VIEWS {
            let root = generation.join(directory);
            for path in index_files(&root)? {
                let relative = path
                    .strip_prefix(&root)
                    .map_err(|error| operation(format!("could not read {}: {error}", path.display())))?
                    .to_string_lossy()
                    .to_string();
                let text = fs::read_to_string(&path).map_err(|error| {
                    operation(format!("could not read {}: {error}", path.display()))
                })?;
                for raw_line in text.split('\n') {
                    let line = raw_line.trim_end_matches('\r');
                    if line.trim().is_empty() {
                        continue;
                    }
                    let adopted = reconstruct_line(view, &relative, line)?;
                    summary.blobs += adopt_blob(store, site_dir, &adopted.checksum)?;
                    match adopted.record {
                        Record::Native(record) => {
                            store.put_native_record(&record)?;
                            summary.native += 1;
                        }
                        Record::Upstream(record) => {
                            store.put_upstream_record(&record)?;
                            summary.upstream += 1;
                        }
                    }
                }
            }
        }
        store.verify_locked()
    })?;
    Ok(summary)
}

fn adopt_blob(store: &Store, site_dir: &Path, checksum: &str) -> Result<usize, Rejection> {
    if !is_sha256_hex(checksum) {
        return Err(integrity(format!(
            "index checksum is not lowercase SHA-256 hex: {checksum}"
        )));
    }
    let present = store.blob_path(checksum).exists();
    let source = site_dir.join("blobs").join(format!("{checksum}.crate"));
    let file = fs::File::open(&source).map_err(|error| {
        integrity(format!("missing blob {}: {error}", source.display()))
    })?;
    let actual = store.ingest_blob(file)?;
    if actual != checksum {
        return Err(integrity(format!(
            "blob {} hashes to {actual}, not the index checksum",
            source.display()
        )));
    }
    Ok(if present { 0 } else { 1 })
}

fn current_generation_dir(site_dir: &Path) -> Result<PathBuf, Rejection> {
    let current = site_dir.join("current");
    let target = fs::read_link(&current).map_err(|error| {
        input(format!(
            "{} is not a current symlink: {error}",
            current.display()
        ))
    })?;
    let target_text = target.to_string_lossy().to_string();
    let generation_name = target_text
        .strip_prefix("generations/")
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or_else(|| integrity(format!("unsafe current target: {target_text}")))?;
    let generation = site_dir.join("generations").join(generation_name);
    if !generation.is_dir() {
        return Err(integrity(format!(
            "current generation is missing: {}",
            generation.display()
        )));
    }
    Ok(generation)
}

fn index_files(root: &Path) -> Result<Vec<PathBuf>, Rejection> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in read_dir(&directory)? {
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                operation(format!("could not inspect {}: {error}", path.display()))
            })?;
            if file_type.is_symlink() {
                return Err(integrity(format!(
                    "unexpected index entry: {}",
                    path.display()
                )));
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                if !(directory.as_path() == root
                    && entry.file_name().to_string_lossy() == "config.json")
                {
                    files.push(path);
                }
            } else {
                return Err(integrity(format!(
                    "unexpected index entry: {}",
                    path.display()
                )));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn read_dir(directory: &Path) -> Result<Vec<fs::DirEntry>, Rejection> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        operation(format!(
            "could not read {}: {error}",
            directory.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            operation(format!(
                "could not read {}: {error}",
                directory.display()
            ))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn operation(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Operation, message)
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
