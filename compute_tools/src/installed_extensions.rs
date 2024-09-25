use compute_api::responses::{InstalledExtension, InstalledExtenstions};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use url::Url;

use anyhow::{anyhow, Result};
use postgres::{Client, NoTls};
use semver::Version;
use tokio::task;
use tracing::{debug, info};

/// We don't reuse get_existing_dbs() just for code clarity
/// and to make database listing query here more explicit.
///
/// Limit the number of databases to 500 to avoid excessive load.
fn list_dbs(client: &mut Client) -> Result<Vec<String>> {
    // `pg_database.datconnlimit = -2` means that the database is in the
    // invalid state
    let databases = client
        .query(
            "SELECT datname FROM pg_database
                WHERE datallowconn
                AND datconnlimit <> - 2
                LIMIT 500",
            &[],
        )?
        .iter()
        .map(|row| {
            let db: String = row.get("datname");
            db
        })
        .collect();

    Ok(databases)
}

/// Connect to every database (see list_dbs above) and get the list of installed extensions.
/// Same extension can be installed in multiple databases with different versions,
/// we only keep the highest and lowest version across all databases.
pub async fn get_installed_extensions(connstr: Url) -> Result<InstalledExtenstions> {
    let mut connstr = connstr.clone();

    task::spawn_blocking(move || {
        let mut client = Client::connect(connstr.as_str(), NoTls)?;
        let databases: Vec<String> = list_dbs(&mut client)?;

        let mut extensions_map: HashMap<String, InstalledExtension> = HashMap::new();
        for db in databases.iter() {
            connstr.set_path(db);
            let mut db_client = Client::connect(connstr.as_str(), NoTls)?;
            let extensions: Vec<(String, String)> = db_client
                .query(
                    "SELECT extname, extversion FROM pg_catalog.pg_extension;",
                    &[],
                )?
                .iter()
                .map(|row| (row.get("extname"), row.get("extversion")))
                .collect();

            for (extname, v) in extensions.iter() {
                // insert extension into the hashmap
                // update the highest_version if new version is higher
                // update the lowest_version if new version is lower
                let version = v.to_string();
                extensions_map
                    .entry(extname.to_string())
                    .and_modify(|e| {
                        let version_sem = SemanticVersion::from_str_safe(&version);
                        let lowest_version_sem = SemanticVersion::from_str_safe(&e.lowest_version);
                        let highest_version_sem =
                            SemanticVersion::from_str_safe(&e.highest_version);

                        debug!(
                            "extname: {}, version: {}, lowest: {}, highest: {}",
                            extname, version, e.lowest_version, e.highest_version
                        );

                        if lowest_version_sem > version_sem {
                            e.lowest_version = version.clone();
                        }
                        if highest_version_sem < version_sem {
                            e.highest_version = version.clone();
                        }

                        // count the number of databases where the extension is installed
                        e.n_databases += 1;
                    })
                    .or_insert(InstalledExtension {
                        extname: extname.to_string(),
                        lowest_version: version.clone(),
                        highest_version: version.clone(),
                        n_databases: 1,
                    });
            }
        }

        Ok(InstalledExtenstions {
            extensions: extensions_map.values().cloned().collect(),
        })
    })
    .await?
}

pub fn log_installed_extensions(connstr: Url) -> Result<()> {
    let connstr = connstr.clone();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create runtime");
    let result = rt
        .block_on(get_installed_extensions(connstr))
        .map_err(|e| anyhow!("failed to get installed extensions: {:?}", e))?;

    info!(
        "[INSTALLED_EXTENSIONS]: {}",
        serde_json::to_string(&result)
            .map_err(|e| anyhow!("failed to deserialize installed extensions: {:?}", e))?
    );
    Ok(())
}

//-------------------------------------------------------------------------
//
// Most of the postgres extensions use 2 part versioning (major.minor)
// Some extensions use 3 part versioning (major.minor.patch)
// Postgres does not enforce any versioning scheme, so we also add
// fallback to raw string comparison.
#[derive(Debug, Clone)]
struct SemanticVersion {
    semver: Option<Version>,
    raw: String,
}

impl SemanticVersion {
    // Helper method to compare versions
    // If both versions are semver, compare them
    // Otherwise fallback to raw string comparison, that's the best we can do
    fn compare(&self, other: &Self) -> Ordering {
        if self.semver.is_some() && other.semver.is_some() {
            self.semver.cmp(&other.semver)
        } else {
            self.raw.cmp(&other.raw)
        }
    }
    fn from_str_safe(s: &str) -> SemanticVersion {
        SemanticVersion {
            semver: Version::parse(s).ok(),
            raw: s.to_string(),
        }
    }
}

impl PartialEq for SemanticVersion {
    fn eq(&self, other: &Self) -> bool {
        if self.semver.is_some() && other.semver.is_some() {
            self.semver == other.semver
        } else {
            self.raw == other.raw
        }
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.semver.is_some() && other.semver.is_some() {
            self.semver.partial_cmp(&other.semver)
        } else {
            Some(self.compare(other))
        }
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.semver.is_some() {
            write!(f, "{}", self.semver.as_ref().unwrap())
        } else {
            write!(f, "{}", self.raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::installed_extensions::SemanticVersion;

    #[test]
    fn test_semantic_version() {
        let v1 = SemanticVersion::from_str_safe("1.0");
        let v2 = SemanticVersion::from_str_safe("1.0.0");
        let v3 = SemanticVersion::from_str_safe("1.1");
        let v4 = SemanticVersion::from_str_safe("2.0");
        let v5 = SemanticVersion::from_str_safe("2.0.1");
        let v6 = SemanticVersion::from_str_safe("2.1.1");
        // This shouldn't happen in real world
        // but let's test that parsing and comparison
        // can handle weird versions too.
        let v7 = SemanticVersion::from_str_safe("2.1a");
        let v8 = SemanticVersion::from_str_safe("2.1x");

        assert!(v1 < v3);
        assert!(v2 <= v3);
        assert!(v3 < v4);
        assert!(v5 > v4);
        assert!(v6 > v5);
        assert!(v6 < v7);
        assert!(v4 < v8);
        assert!(v8 > v7);
    }
}
