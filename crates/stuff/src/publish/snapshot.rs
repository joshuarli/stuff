#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use crate::publish::config::Config;
use crate::publish::r2::pull::{object_etag, safe_relative, PART_SIZE};
use crate::publish::r2::{ListedObject, R2Client};
use crate::publish::{PublishError, Registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedStore {
    pub bucket: String,
    pub prefix: &'static str,
}

impl SharedStore {
    pub fn contains(&self, key: &str) -> bool {
        key.starts_with(self.prefix)
    }
}

pub fn shared_stores(registry: Registry, config: &Config) -> Vec<SharedStore> {
    match registry {
        Registry::Cargo => vec![SharedStore {
            bucket: config.cargo.bucket.clone(),
            prefix: "blobs/",
        }],
        Registry::Npm => vec![SharedStore {
            bucket: config.npm.bucket.clone(),
            prefix: "tarballs/",
        }],
        Registry::Pypi => vec![SharedStore {
            bucket: config.pypi.bucket.clone(),
            prefix: "wheels/",
        }],
        Registry::Oci => vec![SharedStore {
            bucket: config.oci.blobs_bucket.clone(),
            prefix: "sha256/",
        }],
    }
}

pub fn metadata_bucket(registry: Registry, config: &Config) -> String {
    match registry {
        Registry::Cargo => config.cargo.bucket.clone(),
        Registry::Npm => config.npm.bucket.clone(),
        Registry::Pypi => config.pypi.bucket.clone(),
        Registry::Oci => config.oci.bucket.clone(),
    }
}

pub struct Snapshot {
    client: R2Client,
    registry: Registry,
    metadata_bucket: String,
    cache: PathBuf,
    stores: Vec<SharedStore>,
    listings: BTreeMap<String, BTreeMap<String, ListedObject>>,
}

impl Snapshot {
    pub fn open(
        client: R2Client,
        registry: Registry,
        config: &Config,
        cache: PathBuf,
    ) -> Result<Self, PublishError> {
        let stores = shared_stores(registry, config);
        let metadata_bucket = metadata_bucket(registry, config);
        let mut buckets = BTreeSet::new();
        buckets.insert(metadata_bucket.clone());
        for store in &stores {
            buckets.insert(store.bucket.clone());
        }
        let mut listings = BTreeMap::new();
        for bucket in buckets {
            let objects = client.list_objects_v2(&bucket)?;
            listings.insert(
                bucket,
                objects.into_iter().map(|object| (object.key.clone(), object)).collect(),
            );
        }
        Ok(Snapshot {
            client,
            registry,
            metadata_bucket,
            cache,
            stores,
            listings,
        })
    }

    pub fn registry(&self) -> Registry {
        self.registry
    }

    pub fn metadata(&self) -> &BTreeMap<String, ListedObject> {
        &self.listings[&self.metadata_bucket]
    }

    pub fn shared_stores(&self) -> &[SharedStore] {
        &self.stores
    }

    pub fn listing(&self, bucket: &str) -> Option<&BTreeMap<String, ListedObject>> {
        self.listings.get(bucket)
    }

    pub fn shared_objects<'a>(
        &'a self,
        store: &'a SharedStore,
    ) -> impl Iterator<Item = &'a ListedObject> + 'a {
        self.listings
            .get(&store.bucket)
            .into_iter()
            .flat_map(move |objects| {
                objects
                    .values()
                    .filter(move |object| store.contains(&object.key))
            })
    }

    pub fn fetch(&self, bucket: &str, key: &str) -> Result<Vec<u8>, PublishError> {
        let cache_path = self.cache.join(bucket).join(safe_relative(key)?);
        let listed = self.listings.get(bucket).and_then(|objects| objects.get(key));
        if cache_path.is_file() {
            let cached = fs::read(&cache_path).map_err(|error| {
                PublishError::Config(format!(
                    "could not read {}: {error}",
                    cache_path.display()
                ))
            })?;
            match listed {
                Some(listed)
                    if cached.len() as u64 == listed.size
                        && object_etag(&cached, &listed.etag, PART_SIZE) == listed.etag =>
                {
                    return Ok(cached);
                }
                Some(_) => {}
                None => {
                    return Err(PublishError::Integrity(format!(
                        "{bucket}/{key} is not in the live listing"
                    )));
                }
            }
        }
        let listed = listed.ok_or_else(|| {
            PublishError::Integrity(format!("{bucket}/{key} is not in the live listing"))
        })?;
        let bytes = self.client.get_object(bucket, key)?;
        if bytes.len() as u64 != listed.size || object_etag(&bytes, &listed.etag, PART_SIZE) != listed.etag
        {
            return Err(PublishError::Integrity(format!(
                "{bucket}/{key} differs from the live listing"
            )));
        }
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                PublishError::Config(format!("could not create {}: {error}", parent.display()))
            })?;
        }
        fs::write(&cache_path, &bytes).map_err(|error| {
            PublishError::Config(format!("could not write {}: {error}", cache_path.display()))
        })?;
        Ok(bytes)
    }

    pub fn fetch_shared(&self, store: &SharedStore, key: &str) -> Result<Vec<u8>, PublishError> {
        if !store.contains(key) {
            return Err(PublishError::Usage(format!(
                "{key} is not in the {} shared store",
                store.prefix
            )));
        }
        self.fetch(&store.bucket, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_registry_names_its_shared_store() {
        let config = Config::default();
        assert_eq!(
            shared_stores(Registry::Cargo, &config),
            vec![SharedStore {
                bucket: "stuff-mycrates".to_string(),
                prefix: "blobs/",
            }]
        );
        assert_eq!(
            shared_stores(Registry::Npm, &config),
            vec![SharedStore {
                bucket: "stuff-mynpm".to_string(),
                prefix: "tarballs/",
            }]
        );
        assert_eq!(
            shared_stores(Registry::Pypi, &config),
            vec![SharedStore {
                bucket: "stuff-mypypi".to_string(),
                prefix: "wheels/",
            }]
        );
        assert_eq!(
            shared_stores(Registry::Oci, &config),
            vec![SharedStore {
                bucket: "stuff-oci-blobs".to_string(),
                prefix: "sha256/",
            }]
        );
    }

    #[test]
    fn the_metadata_bucket_is_the_registry_bucket() {
        let config = Config::default();
        assert_eq!(metadata_bucket(Registry::Cargo, &config), "stuff-mycrates");
        assert_eq!(metadata_bucket(Registry::Npm, &config), "stuff-mynpm");
        assert_eq!(metadata_bucket(Registry::Pypi, &config), "stuff-mypypi");
        assert_eq!(metadata_bucket(Registry::Oci, &config), "stuff-oci");
    }

    #[test]
    fn shared_store_prefixes_accept_only_their_own_keys() {
        let store = SharedStore {
            bucket: "bucket".to_string(),
            prefix: "tarballs/",
        };
        assert!(store.contains("tarballs/a.tgz"));
        assert!(!store.contains("blobs/a.crate"));
        assert!(!store.contains("generations/g/tarballs/a.tgz"));
    }
}
