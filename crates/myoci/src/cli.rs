#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::import::{import_from, import_layout};
use crate::inspect::inspect;
use crate::publish::{adopt, publish, DEFAULT_PUBLIC_URL};
use crate::state::State;
use crate::verify::verify_state;
use stuff_oci::{Digest, RepositoryName, Tag};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Init,
    Import,
    Inspect,
    Publish,
    Verify,
    Adopt,
    Tag,
    Untag,
    Serve,
    Writer,
    Bench,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub state_dir: String,
    pub layout: Option<String>,
    pub from: Option<String>,
    pub repository: Option<String>,
    pub tag: Option<String>,
    pub digest: Option<String>,
    pub site_dir: Option<String>,
    pub public_url: Option<String>,
    pub bind: Option<String>,
    pub max_connections: Option<usize>,
    pub base_url: Option<String>,
    pub runs: Option<usize>,
    pub label: Option<String>,
    pub output: Option<String>,
    pub push: bool,
    pub push_token_file: Option<String>,
    pub push_secret_file: Option<String>,
    pub writer_url: Option<String>,
    pub r2_bucket: Option<String>,
    pub deploy_dir: Option<String>,
    pub json: bool,
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    let Some(command) = argv.first() else {
        return Err(Error::Usage("a command is required".to_string()));
    };
    let command = match command.as_str() {
        "init" => Command::Init,
        "import" => Command::Import,
        "inspect" => Command::Inspect,
        "publish" => Command::Publish,
        "verify" => Command::Verify,
        "adopt" => Command::Adopt,
        "tag" => Command::Tag,
        "untag" => Command::Untag,
        "serve" => Command::Serve,
        "writer" => Command::Writer,
        "bench" => Command::Bench,
        other => return Err(Error::Usage(format!("unknown command {other}"))),
    };
    let mut parsed = Parsed {
        command,
        state_dir: String::new(),
        layout: None,
        from: None,
        repository: None,
        tag: None,
        digest: None,
        site_dir: None,
        public_url: None,
        bind: None,
        max_connections: None,
        base_url: None,
        runs: None,
        label: None,
        output: None,
        push: false,
        push_token_file: None,
        push_secret_file: None,
        writer_url: None,
        r2_bucket: None,
        deploy_dir: None,
        json: false,
    };
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--state-dir" => parsed.state_dir = take(argv, &mut i, "--state-dir")?,
            "--layout" => parsed.layout = Some(take(argv, &mut i, "--layout")?),
            "--from" => parsed.from = Some(take(argv, &mut i, "--from")?),
            "--repository" | "--repo" => parsed.repository = Some(take(argv, &mut i, "--repository")?),
            "--tag" => parsed.tag = Some(take(argv, &mut i, "--tag")?),
            "--digest" => parsed.digest = Some(take(argv, &mut i, "--digest")?),
            "--site-dir" => parsed.site_dir = Some(take(argv, &mut i, "--site-dir")?),
            "--public-url" => parsed.public_url = Some(take(argv, &mut i, "--public-url")?),
            "--bind" => parsed.bind = Some(take(argv, &mut i, "--bind")?),
            "--max-connections" => {
                let value = take(argv, &mut i, "--max-connections")?;
                parsed.max_connections = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| Error::Usage("--max-connections must be an integer".to_string()))?,
                );
            }
            "--base-url" => parsed.base_url = Some(take(argv, &mut i, "--base-url")?),
            "--runs" => {
                let value = take(argv, &mut i, "--runs")?;
                parsed.runs = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| Error::Usage("--runs must be an integer".to_string()))?,
                );
            }
            "--label" => parsed.label = Some(take(argv, &mut i, "--label")?),
            "--output" => parsed.output = Some(take(argv, &mut i, "--output")?),
            "--push" => parsed.push = true,
            "--push-token-file" => parsed.push_token_file = Some(take(argv, &mut i, "--push-token-file")?),
            "--push-secret-file" | "--secret-file" => {
                parsed.push_secret_file = Some(take(argv, &mut i, "--push-secret-file")?)
            }
            "--writer-url" => parsed.writer_url = Some(take(argv, &mut i, "--writer-url")?),
            "--r2-bucket" => parsed.r2_bucket = Some(take(argv, &mut i, "--r2-bucket")?),
            "--deploy-dir" => parsed.deploy_dir = Some(take(argv, &mut i, "--deploy-dir")?),
            "--json" => parsed.json = true,
            other => return Err(Error::Usage(format!("unknown option {other}"))),
        }
        i += 1;
    }
    if !matches!(command, Command::Serve | Command::Bench) && parsed.state_dir.is_empty() {
        return Err(Error::Usage("--state-dir is required".to_string()));
    }
    let allows = |allowed: &[&str]| -> Result<()> {
        let set = [("--layout", parsed.layout.is_some()), ("--from", parsed.from.is_some()), ("--repository", parsed.repository.is_some()), ("--tag", parsed.tag.is_some()), ("--digest", parsed.digest.is_some()), ("--site-dir", parsed.site_dir.is_some()), ("--public-url", parsed.public_url.is_some()), ("--bind", parsed.bind.is_some()), ("--max-connections", parsed.max_connections.is_some()), ("--base-url", parsed.base_url.is_some()), ("--runs", parsed.runs.is_some()), ("--label", parsed.label.is_some()), ("--output", parsed.output.is_some()), ("--push", parsed.push), ("--push-token-file", parsed.push_token_file.is_some()), ("--push-secret-file", parsed.push_secret_file.is_some()), ("--writer-url", parsed.writer_url.is_some()), ("--r2-bucket", parsed.r2_bucket.is_some()), ("--deploy-dir", parsed.deploy_dir.is_some()), ("--json", parsed.json)];
        for (flag, present) in set {
            if present && !allowed.contains(&flag) {
                return Err(Error::Usage(format!("{flag} is not valid for this command")));
            }
        }
        Ok(())
    };
    match command {
        Command::Init => allows(&[])?,
        Command::Import => {
            allows(&["--layout", "--from", "--repository", "--tag"])?;
            if parsed.repository.is_none() {
                return Err(Error::Usage("import requires --repository".to_string()));
            }
            if parsed.layout.is_some() == parsed.from.is_some() {
                return Err(Error::Usage(
                    "import requires exactly one of --layout or --from".to_string(),
                ));
            }
        }
        Command::Inspect => allows(&["--json"])?,
        Command::Publish => {
            allows(&["--site-dir", "--public-url"])?;
            if parsed.site_dir.is_none() {
                return Err(Error::Usage("publish requires --site-dir".to_string()));
            }
        }
        Command::Verify => allows(&[])?,
        Command::Adopt => {
            allows(&["--site-dir"])?;
            if parsed.site_dir.is_none() {
                return Err(Error::Usage("adopt requires --site-dir".to_string()));
            }
        }
        Command::Tag => {
            allows(&["--repository", "--tag", "--digest"])?;
            for (flag, value) in [("--repository", &parsed.repository), ("--tag", &parsed.tag), ("--digest", &parsed.digest)] {
                if value.is_none() {
                    return Err(Error::Usage(format!("tag requires {flag}")));
                }
            }
        }
        Command::Untag => {
            allows(&["--repository", "--tag"])?;
            for (flag, value) in [("--repository", &parsed.repository), ("--tag", &parsed.tag)] {
                if value.is_none() {
                    return Err(Error::Usage(format!("untag requires {flag}")));
                }
            }
        }
        Command::Serve => {
            allows(&[
                "--site-dir",
                "--bind",
                "--max-connections",
                "--push",
                "--push-token-file",
                "--push-secret-file",
                "--writer-url",
            ])?;
            if parsed.site_dir.is_none() {
                return Err(Error::Usage("serve requires --site-dir".to_string()));
            }
            if parsed.push {
                for (flag, present) in [
                    ("--state-dir", !parsed.state_dir.is_empty()),
                    ("--writer-url", parsed.writer_url.is_some()),
                    ("--push-token-file", parsed.push_token_file.is_some() || std::env::var("STUFF_OCI_PUSH_TOKEN").is_ok()),
                    ("--push-secret-file", parsed.push_secret_file.is_some() || std::env::var("STUFF_OCI_PUSH_SECRET").is_ok()),
                ] {
                    if !present {
                        return Err(Error::Usage(format!("serve --push requires {flag}")));
                    }
                }
            } else if parsed.push_token_file.is_some()
                || parsed.push_secret_file.is_some()
                || parsed.writer_url.is_some()
            {
                return Err(Error::Usage("serve --push options require --push".to_string()));
            }
        }
        Command::Writer => {
            allows(&["--bind", "--max-connections", "--push-secret-file", "--r2-bucket", "--deploy-dir"])?;
            if !parsed.push_secret_file.is_some() && std::env::var("STUFF_OCI_PUSH_SECRET").is_err() {
                return Err(Error::Usage(
                    "writer requires --push-secret-file or STUFF_OCI_PUSH_SECRET".to_string(),
                ));
            }
        }
        Command::Bench => {
            allows(&["--base-url", "--repository", "--tag", "--runs", "--label", "--output"])?;
            for (flag, present) in [
                ("--base-url", parsed.base_url.is_some()),
                ("--repository", parsed.repository.is_some()),
                ("--tag", parsed.tag.is_some()),
            ] {
                if !present {
                    return Err(Error::Usage(format!("bench requires {flag}")));
                }
            }
        }
    }
    Ok(parsed)
}

fn take(argv: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    argv.get(*i)
        .cloned()
        .ok_or_else(|| Error::Usage(format!("{flag} requires a value")))
}

pub fn run(argv: &[String]) -> Result<()> {
    let parsed = parse_args(argv)?;
    let state = State::new(PathBuf::from(&parsed.state_dir));
    match parsed.command {
        Command::Init => {
            state.init()?;
            println!("initialized {}", state.root().display());
        }
        Command::Import => {
            let repository = repository(&parsed)?;
            let tag = tag(&parsed)?;
            let summary = match (&parsed.layout, &parsed.from) {
                (Some(layout), None) => import_layout(&state, std::path::Path::new(layout), &repository, tag.as_ref())?,
                (None, Some(from)) => import_from(&state, from, &repository, tag.as_ref())?,
                _ => unreachable!("validated by parse_args"),
            };
            println!(
                "imported {} as {}:{} ({} manifests, {} blobs, {} referrers)",
                summary.root,
                repository.as_str(),
                summary.tag.as_str(),
                summary.manifests,
                summary.blobs,
                summary.referrers
            );
        }
        Command::Inspect => {
            let inspection = inspect(&state)?;
            if parsed.json {
                print!("{}", inspection.render_json());
            } else {
                println!("tags: {}", inspection.tags.join(" "));
                println!("manifests: {}", inspection.manifests.join(" "));
                println!("blobs: {}", inspection.blobs.join(" "));
                println!("referrers: {}", inspection.referrers.join(" "));
            }
        }
        Command::Publish => {
            let site_dir = parsed.site_dir.as_deref().expect("validated");
            let public_url = parsed.public_url.as_deref().unwrap_or(DEFAULT_PUBLIC_URL);
            let summary = publish(&state, std::path::Path::new(site_dir), public_url)?;
            if summary.no_op {
                println!("Already published generation {}", summary.generation);
            } else {
                println!(
                    "Published generation {} ({} repositories, {} manifests, {} blobs, {} referrers)",
                    summary.generation,
                    summary.repositories,
                    summary.manifests,
                    summary.blobs,
                    summary.referrers
                );
            }
        }
        Command::Verify => {
            let summary = verify_state(&state)?;
            println!(
                "verified {} repositories, {} manifests, {} blobs, {} referrers",
                summary.repositories, summary.manifests, summary.blobs, summary.referrers
            );
        }
        Command::Adopt => {
            let site_dir = parsed.site_dir.as_deref().expect("validated");
            let summary = adopt(&state, std::path::Path::new(site_dir))?;
            println!(
                "adopted generation {} ({} repositories, {} manifests, {} blobs, {} referrers)",
                summary.generation, summary.repositories, summary.manifests, summary.blobs, summary.referrers
            );
        }
        Command::Tag => {
            let repository = repository(&parsed)?;
            let tag = tag(&parsed)?.expect("validated");
            let digest_text = parsed.digest.as_deref().expect("validated");
            let digest = Digest::parse(digest_text)
                .map_err(|error| Error::Input(format!("--digest: {error}")))?;
            state.set_tag(&repository, tag.clone(), &digest)?;
            println!("tagged {}:{} as {}", repository.as_str(), tag.as_str(), digest);
        }
        Command::Untag => {
            let repository = repository(&parsed)?;
            let tag = tag(&parsed)?.expect("validated");
            if !state.remove_tag(&repository, &tag)? {
                return Err(Error::Input(format!(
                    "{} has no tag {}",
                    repository.as_str(),
                    tag.as_str()
                )));
            }
            println!("removed {}:{}", repository.as_str(), tag.as_str());
        }
        Command::Serve => {
            let site_dir = parsed.site_dir.as_deref().expect("validated");
            let site_path = PathBuf::from(site_dir);
            let push = if parsed.push {
                let state_dir = parsed.state_dir.as_str();
                let state = State::new(PathBuf::from(state_dir));
                state.init()?;
                let token = String::from_utf8(read_secret(
                    parsed.push_token_file.as_deref(),
                    "STUFF_OCI_PUSH_TOKEN",
                )?)
                .map_err(|_| Error::Input("--push-token-file must be UTF-8".to_string()))?;
                Some(crate::push::PushOptions {
                    state,
                    site_dir: site_path.clone(),
                    public_url: DEFAULT_PUBLIC_URL.to_string(),
                    token,
                    secret: read_secret(parsed.push_secret_file.as_deref(), "STUFF_OCI_PUSH_SECRET")?,
                    writer_url: parsed.writer_url.clone().expect("validated"),
                })
            } else {
                None
            };
            crate::serve::run(crate::serve::ServeOptions {
                site_dir: site_path,
                bind: parsed.bind.clone().unwrap_or_else(|| "127.0.0.1:0".to_string()),
                max_connections: parsed
                    .max_connections
                    .unwrap_or(crate::serve::DEFAULT_MAX_CONNECTIONS),
                push,
            })?;
        }
        Command::Writer => {
            let state_dir = parsed.state_dir.as_str();
            let state = State::new(PathBuf::from(state_dir));
            state.init()?;
            crate::writer::run(crate::writer::WriterOptions {
                bind: parsed.bind.clone().unwrap_or_else(|| "127.0.0.1:0".to_string()),
                state,
                secret: read_secret(parsed.push_secret_file.as_deref(), "STUFF_OCI_PUSH_SECRET")?,
                max_connections: parsed
                    .max_connections
                    .unwrap_or(crate::writer::DEFAULT_MAX_CONNECTIONS),
                sink: match parsed.r2_bucket.clone() {
                    Some(bucket) => crate::writer::WriterSink::R2 {
                        bucket,
                        deploy_dir: PathBuf::from(
                            parsed.deploy_dir.as_deref().unwrap_or("deploy"),
                        ),
                    },
                    None => crate::writer::WriterSink::Local,
                },
            })?;
        }
        Command::Bench => {
            let repository = repository(&parsed)?;
            let tag = tag(&parsed)?.ok_or_else(|| Error::Usage("bench requires --tag".to_string()))?;
            let base_url = parsed.base_url.as_deref().expect("validated");
            let output = PathBuf::from(
                parsed
                    .output
                    .as_deref()
                    .unwrap_or("docs/oci-benchmarks.md"),
            );
            crate::bench::run(&crate::bench::BenchOptions {
                base_url: base_url.to_string(),
                repository,
                tag,
                output,
                runs: parsed.runs.unwrap_or(5),
                label: parsed
                    .label
                    .clone()
                    .unwrap_or_else(|| "myoci serve".to_string()),
            })?;
        }
    }
    Ok(())
}

fn repository(parsed: &Parsed) -> Result<RepositoryName> {
    RepositoryName::parse(parsed.repository.as_deref().expect("validated"))
        .map_err(|error| Error::Input(format!("--repository: {error}")))
}

fn read_secret(file: Option<&str>, env_name: &str) -> Result<Vec<u8>> {
    let mut bytes = match file {
        Some(path) => std::fs::read(path)
            .map_err(|error| Error::Input(format!("could not read {path}: {error}")))?,
        None => match std::env::var(env_name) {
            Ok(value) => value.into_bytes(),
            Err(_) => return Err(Error::Input(format!("{env_name} is not set"))),
        },
    };
    while matches!(bytes.last(), Some(b'\n' | b'\r' | b' ' | b'\t')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(Error::Input(format!("{env_name} is empty")));
    }
    Ok(bytes)
}

fn tag(parsed: &Parsed) -> Result<Option<Tag>> {
    parsed
        .tag
        .as_deref()
        .map(Tag::parse)
        .transpose()
        .map_err(|error| Error::Input(format!("--tag: {error}")))
}

pub fn print_help() {
    println!(
        "myoci {version}\n\
         \n\
         usage: myoci <command> --state-dir DIR [options]\n\
         \n\
         commands:\n\
         \x20 init                         create the state directories\n\
         \x20 import --layout DIR          import an OCI image layout\n\
         \x20 import --from REFERENCE      import through oras {oras}\n\
         \x20 inspect [--json]             list tags, manifests, blobs, referrers\n\
         \x20 publish --site-dir DIR       publish the state as a generation\n\
         \x20 verify                       verify every blob, manifest, tag, referrer\n\
         \x20 adopt --site-dir DIR         rebuild state from a published generation\n\
         \x20 tag --repository NAME --tag TAG --digest DIGEST\n\
         \x20 untag --repository NAME --tag TAG\n\
         \x20 serve --site-dir DIR        serve the read API ([--bind ADDR] [--max-connections N])\n\
         \x20 serve --push               enable writes; requires --state-dir and --writer-url\n\
         \x20 writer                     run the blob writer ([--bind ADDR] [--max-connections N])\n\
         \x20 bench --base-url URL --repo NAME --tag TAG [--output FILE] [--runs N] [--label TEXT]\n\
         \n\
         import options:\n\
         \x20 --repository NAME            destination repository (required)\n\
         \x20 --tag TAG                    tag to assign (default: layout ref name or latest)\n\
         \n\
         publish options:\n\
         \x20 --public-url URL             origin written to llms.txt (default: {origin})",
        version = env!("CARGO_PKG_VERSION"),
        oras = crate::import::ORAS_VERSION,
        origin = DEFAULT_PUBLIC_URL
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_commands_and_rejects_ambiguous_imports() {
        let parsed = parse_args(&args(&[
            "import",
            "--state-dir",
            "state",
            "--layout",
            "layout",
            "--repository",
            "example/app",
            "--tag",
            "stable",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::Import);
        assert_eq!(parsed.repository.as_deref(), Some("example/app"));
        assert!(parse_args(&args(&["import", "--state-dir", "s", "--repository", "a/b"])).is_err());
        assert!(parse_args(&args(&[
            "import",
            "--state-dir",
            "s",
            "--layout",
            "l",
            "--from",
            "f",
            "--repository",
            "a/b"
        ]))
        .is_err());
        assert!(parse_args(&args(&["init"])).is_err());
        assert!(parse_args(&args(&["inspect", "--state-dir", "s", "--json"])).is_ok());
    }

    #[test]
    fn parses_publish_tag_and_untag() {
        let parsed = parse_args(&args(&[
            "publish",
            "--state-dir",
            "s",
            "--site-dir",
            "site",
            "--public-url",
            "https://oci.example.test",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::Publish);
        let digest = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
        assert!(parse_args(&args(&[
            "tag",
            "--state-dir",
            "s",
            "--repository",
            "example/app",
            "--tag",
            "stable",
            "--digest",
            digest
        ]))
        .is_ok());
        assert!(parse_args(&args(&[
            "tag",
            "--state-dir",
            "s",
            "--repository",
            "example/app",
            "--tag",
            "stable"
        ]))
        .is_err());
        assert!(parse_args(&args(&[
            "untag",
            "--state-dir",
            "s",
            "--repository",
            "example/app",
            "--tag",
            "stable"
        ]))
        .is_ok());
    }
}
