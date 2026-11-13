use std::io::Write;

use crate::archive::Member;
use crate::error::{Error, Result};

const DOS_EPOCH: rawzip::time::UtcDateTime = match rawzip::time::UtcDateTime::from_components(1980, 1, 1, 0, 0, 0, 0) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) fn write_zip(members: &[Member]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut archive = rawzip::ZipArchiveWriter::new(&mut out);
        for member in members {
            let (mut entry, config) = archive
                .new_file(member.path.as_str())
                .compression_method(rawzip::CompressionMethod::DEFLATE)
                .unix_permissions(member.mode)
                .last_modified(DOS_EPOCH)
                .start()
                .map_err(|error| Error::operation(format!("could not start zip member {}: {error}", member.path)))?;
            let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
            let mut writer = config.wrap(encoder);
            writer
                .write_all(&member.data)
                .map_err(|error| Error::operation(format!("could not write zip member {}: {error}", member.path)))?;
            let (encoder, descriptor) = writer
                .finish()
                .map_err(|error| Error::operation(format!("could not finish zip member {}: {error}", member.path)))?;
            encoder
                .finish()
                .map_err(|error| Error::operation(format!("could not finish zip member {}: {error}", member.path)))?;
            entry
                .finish(descriptor)
                .map_err(|error| Error::operation(format!("could not finish zip member {}: {error}", member.path)))?;
        }
        archive
            .finish()
            .map_err(|error| Error::operation(format!("could not finish zip archive: {error}")))?;
    }
    Ok(out)
}
