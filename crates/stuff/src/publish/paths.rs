#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::publish::transaction::LOCK_FILE;
use crate::publish::{Environment, Registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_root: PathBuf,
    cache_root: PathBuf,
    state_root: PathBuf,
}

impl Paths {
    pub fn from_environment(env: &Environment) -> Self {
        let env = |name: &str| env.get(name).map(str::to_string);
        let home = env("HOME").map(PathBuf::from).unwrap_or_default();
        let config_root = env("STUFF_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env("XDG_CONFIG_HOME").map(|root| PathBuf::from(root).join("stuff")))
            .unwrap_or_else(|| home.join(".config").join("stuff"));
        let cache_root = env("STUFF_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env("XDG_CACHE_HOME").map(|root| PathBuf::from(root).join("stuff")))
            .unwrap_or_else(|| home.join(".cache").join("stuff"));
        let state_root = env("STUFF_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env("XDG_STATE_HOME").map(|root| PathBuf::from(root).join("stuff")))
            .unwrap_or_else(|| home.join(".local").join("state").join("stuff"));
        Paths { config_root, cache_root, state_root }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_root.join("config.json")
    }

    pub fn cache_dir(&self, registry: &str) -> PathBuf {
        self.cache_root.join(registry)
    }

    pub fn state_dir(&self, registry: &str) -> PathBuf {
        self.state_root.join(registry)
    }

    /// Directory holding a registry's publication record and publish lock.
    /// Every writer of that registry, including the `deploy/` maintenance
    /// helpers through `stuff lock-path`, must lock the file inside it.
    pub fn publish_state_dir(&self, registry: Registry) -> PathBuf {
        match registry {
            Registry::Oci => self.state_dir("oci").join("transaction"),
            Registry::Cargo | Registry::Npm | Registry::Pypi | Registry::Mirror => {
                self.state_dir(registry.as_str())
            }
        }
    }

    pub fn publish_lock(&self, registry: Registry) -> PathBuf {
        self.publish_state_dir(registry).join(LOCK_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Environment {
        Environment::from_pairs(pairs.iter().copied())
    }

    #[test]
    fn home_defaults_apply_when_nothing_is_set() {
        let paths = Paths::from_environment(&env(&[("HOME", "/home/person")]));
        assert_eq!(paths.config_file(), PathBuf::from("/home/person/.config/stuff/config.json"));
        assert_eq!(paths.cache_dir("cargo"), PathBuf::from("/home/person/.cache/stuff/cargo"));
        assert_eq!(
            paths.state_dir("oci"),
            PathBuf::from("/home/person/.local/state/stuff/oci")
        );
    }

    #[test]
    fn publish_locks_live_in_each_registry_publication_state() {
        let paths = Paths::from_environment(&env(&[("STUFF_STATE_HOME", "/s")]));
        assert_eq!(paths.publish_lock(Registry::Cargo), PathBuf::from("/s/cargo/publish.lock"));
        assert_eq!(paths.publish_lock(Registry::Npm), PathBuf::from("/s/npm/publish.lock"));
        assert_eq!(paths.publish_lock(Registry::Pypi), PathBuf::from("/s/pypi/publish.lock"));
        assert_eq!(paths.publish_lock(Registry::Oci), PathBuf::from("/s/oci/transaction/publish.lock"));
        assert_eq!(paths.publish_lock(Registry::Mirror), PathBuf::from("/s/mirror/publish.lock"));
    }

    #[test]
    fn xdg_overrides_home() {
        let paths = Paths::from_environment(&env(&[
            ("HOME", "/home/person"),
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
            ("XDG_STATE_HOME", "/xdg/state"),
        ]));
        assert_eq!(paths.config_file(), PathBuf::from("/xdg/config/stuff/config.json"));
        assert_eq!(paths.cache_dir("npm"), PathBuf::from("/xdg/cache/stuff/npm"));
        assert_eq!(paths.state_dir("pypi"), PathBuf::from("/xdg/state/stuff/pypi"));
    }

    #[test]
    fn stuff_overrides_xdg_and_home() {
        let paths = Paths::from_environment(&env(&[
            ("HOME", "/home/person"),
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
            ("XDG_STATE_HOME", "/xdg/state"),
            ("STUFF_CONFIG_HOME", "/stuff/config"),
            ("STUFF_CACHE_HOME", "/stuff/cache"),
            ("STUFF_STATE_HOME", "/stuff/state"),
        ]));
        assert_eq!(paths.config_file(), PathBuf::from("/stuff/config/config.json"));
        assert_eq!(paths.cache_dir("cargo"), PathBuf::from("/stuff/cache/cargo"));
        assert_eq!(paths.state_dir("cargo"), PathBuf::from("/stuff/state/cargo"));
    }

    #[test]
    fn each_registry_gets_its_own_cache_and_state_directory() {
        let paths = Paths::from_environment(&env(&[("HOME", "/home/person")]));
        assert_eq!(paths.cache_dir("cargo"), PathBuf::from("/home/person/.cache/stuff/cargo"));
        assert_eq!(paths.cache_dir("npm"), PathBuf::from("/home/person/.cache/stuff/npm"));
        assert_eq!(paths.cache_dir("pypi"), PathBuf::from("/home/person/.cache/stuff/pypi"));
        assert_eq!(paths.cache_dir("oci"), PathBuf::from("/home/person/.cache/stuff/oci"));
        assert_eq!(paths.cache_dir("mirror"), PathBuf::from("/home/person/.cache/stuff/mirror"));
    }
}
