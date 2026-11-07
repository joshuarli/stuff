#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::publish::inventory::{prepare_first_party, FirstPartyOptions};
use crate::publish::npm::run_publication;
use crate::publish::r2::sigv4::civil_from_days;
use crate::publish::{Environment, Options, Outcome, PublishError};
use mynpm::publish::{publish_first_party, PublishOptions};

use crate::publish::inventory::library_error;

pub fn rfc3339_millis(since_epoch: std::time::Duration) -> String {
    let seconds = since_epoch.as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60,
        since_epoch.subsec_millis()
    )
}

fn resolve(tarball: &str) -> Result<PathBuf, PublishError> {
    let path = PathBuf::from(tarball);
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| PublishError::Config(format!("could not resolve the working directory: {error}")))
}

pub fn publish(
    tarballs: &[String],
    options: Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let tarballs: Vec<PathBuf> = tarballs.iter().map(|tarball| resolve(tarball)).collect::<Result<_, _>>()?;
    let published_at = rfc3339_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| PublishError::Config(format!("the system clock is before 1970: {error}")))?,
    );
    run_publication(options, environment, |snapshot, publication| {
        let records = prepare_first_party(
            snapshot,
            &FirstPartyOptions {
                bucket: publication.bucket,
                store: &publication.store,
                site_dir: &publication.site_dir,
                state_dir: &publication.state_dir,
                tarballs: &tarballs,
                library: &publication.library,
                published_at: &published_at,
            },
        )?;
        publish_first_party(
            &PublishOptions {
                state_directory: &publication.state_dir,
                site_directory: &publication.site_dir,
                config: &publication.library,
                render_base_url: None,
            },
            &records,
        )
        .map_err(library_error)?;
        let mut packages: Vec<String> =
            records.iter().map(|record| format!("{}@{}", record.name, record.version)).collect();
        packages.sort();
        Ok(packages)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_rfc3339_utc_with_milliseconds() {
        assert_eq!(rfc3339_millis(Duration::from_millis(0)), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_millis(Duration::from_millis(1_762_473_600_123)),
            "2025-11-07T00:00:00.123Z"
        );
    }
}
