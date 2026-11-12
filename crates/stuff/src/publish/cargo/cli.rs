#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use crate::cli::parse_args;
    use crate::error::Error;
    use crate::publish::{Options, PublishRequest, Request};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn cargo_request(values: &[&str]) -> (crate::publish::CargoPublish, Options) {
        match parse_args(&args(values)).unwrap().request.unwrap() {
            Request::Publish {
                request: PublishRequest::Cargo(cargo),
                options,
            } => (cargo, options),
            other => panic!("{other:?}"),
        }
    }

    fn usage_exit(values: &[&str]) -> i32 {
        let error = parse_args(&args(values)).unwrap_err();
        assert!(matches!(error, Error::Usage(_)), "{values:?}: {error}");
        error.exit_code()
    }

    #[test]
    fn the_documented_cargo_form_parses() {
        let (cargo, options) = cargo_request(&[
            "publish",
            "cargo",
            "-p",
            "alpha",
            "-p",
            "beta",
            "--manifest-path",
            "Cargo.toml",
            "--registry",
            "stuff",
            "--dry-run",
            "--json",
        ]);
        assert_eq!(cargo.packages, vec!["alpha", "beta"]);
        assert_eq!(cargo.manifest_path.as_deref(), Some("Cargo.toml"));
        assert_eq!(cargo.registry.as_deref(), Some("stuff"));
        assert_eq!(
            options,
            Options {
                dry_run: true,
                json: true,
                verbose: false,
            }
        );
    }

    #[test]
    fn repeated_packages_keep_the_requested_order() {
        let (cargo, _) = cargo_request(&[
            "publish", "cargo", "-p", "gamma", "-p", "alpha", "-p", "beta", "-p", "gamma",
        ]);
        assert_eq!(cargo.packages, vec!["gamma", "alpha", "beta", "gamma"]);
    }

    #[test]
    fn unknown_flags_exit_two() {
        for values in [
            &["publish", "cargo", "-p", "alpha", "--bogus"][..],
            &["publish", "cargo", "-p", "alpha", "--allow-drops"][..],
            &["publish", "cargo", "-p", "alpha", "--image", "repo=tag"][..],
        ] {
            assert_eq!(usage_exit(values), 2, "{values:?}");
        }
    }

    #[test]
    fn duplicate_flags_exit_two() {
        for values in [
            &[
                "publish", "cargo", "-p", "alpha", "--registry", "one", "--registry", "two",
            ][..],
            &[
                "publish",
                "cargo",
                "-p",
                "alpha",
                "--manifest-path",
                "a/Cargo.toml",
                "--manifest-path",
                "b/Cargo.toml",
            ][..],
            &["publish", "cargo", "-p", "alpha", "--dry-run", "--dry-run"][..],
            &["publish", "cargo", "-p", "alpha", "--json", "--json"][..],
            &["publish", "cargo", "-p", "alpha", "--verbose", "--verbose"][..],
        ] {
            assert_eq!(usage_exit(values), 2, "{values:?}");
        }
    }

    #[test]
    fn repeated_lockfiles_parse_into_the_mirror_request() {
        let parsed = parse_args(&args(&[
            "publish", "cargo", "--lockfile", "a/Cargo.lock", "--lockfile", "b/Cargo.lock", "--dry-run",
        ]))
        .unwrap()
        .request
        .unwrap();
        match parsed {
            Request::Publish {
                request: PublishRequest::CargoLockfiles(lockfiles),
                options,
            } => {
                assert_eq!(lockfiles, vec!["a/Cargo.lock", "b/Cargo.lock"]);
                assert!(options.dry_run);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lockfiles_do_not_combine_with_workspace_publishing() {
        for values in [
            &["publish", "cargo", "--lockfile", "Cargo.lock", "-p", "alpha"][..],
            &["publish", "cargo", "--lockfile", "Cargo.lock", "--manifest-path", "Cargo.toml"][..],
            &["publish", "cargo", "--lockfile", "Cargo.lock", "--registry", "stuff"][..],
            &["publish", "cargo", "--lockfile"][..],
            &["publish", "cargo", "--lockfile", ""][..],
        ] {
            assert_eq!(usage_exit(values), 2, "{values:?}");
        }
    }

    #[test]
    fn a_missing_package_is_a_usage_error() {
        assert_eq!(usage_exit(&["publish", "cargo"]), 2);
        assert_eq!(usage_exit(&["publish", "cargo", "-p"]), 2);
    }
}
