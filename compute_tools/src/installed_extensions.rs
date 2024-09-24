use compute_api::responses::{InstalledExtension, InstalledExtenstions};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use url::Url;

use anyhow::Result;
use postgres::{Client, NoTls};
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
                        // convert version to SemanticVersion
                        let version_sem =
                            SemanticVersion::from_str(&version).expect("failed to parse version");
                        // use SemanticVersion to compare versions
                        let lowest_version_sem = SemanticVersion::from_str(&e.lowest_version)
                            .expect("failed to parse lowest version");
                        let highest_version_sem = SemanticVersion::from_str(&e.highest_version)
                            .expect("failed to parse highest version");

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
        .expect("failed to get installed extensions");

    info!(
        "[INSTALLED_EXTENSIONS]: {}",
        serde_json::to_string(&result).with_context(|| "failed to serialize extensions list")?
    );
    Ok(())
}

//-------------------------------------------------------------------------
//
// Most of the postgres extensions use 2 part versioning (major.minor)
// Some extensions use 3 part versioning (major.minor.patch)
//

#[derive(Debug, Clone, Copy)]
struct SemanticVersion {
    major: u32,
    minor: u32,
    patch: Option<u32>, // Make patch optional
}

impl SemanticVersion {
    // Helper method to compare versions
    fn compare(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.unwrap_or(0).cmp(&other.patch.unwrap_or(0)))
    }
}

impl FromStr for SemanticVersion {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return Err("Version must have 1 to 3 parts separated by periods".to_string());
        }

        let major = parts
            .first()
            .ok_or("Missing major version")?
            .parse::<u32>()
            .map_err(|_| "Invalid major version".to_string())?;
        let minor = parts
            .get(1)
            .ok_or("Missing minor version")?
            .parse::<u32>()
            .map_err(|_| "Invalid minor version".to_string())?;
        let patch = parts
            .get(2)
            .map(|&p| {
                p.parse::<u32>()
                    .map_err(|_| "Invalid patch version".to_string())
            })
            .transpose()?;

        Ok(SemanticVersion {
            major,
            minor,
            patch,
        })
    }
}

impl PartialEq for SemanticVersion {
    fn eq(&self, other: &Self) -> bool {
        self.compare(other) == Ordering::Equal
    }
}

impl PartialOrd for SemanticVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.compare(other))
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.patch {
            Some(patch) => write!(f, "{}.{}.{}", self.major, self.minor, patch),
            None => write!(f, "{}.{}", self.major, self.minor),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::installed_extensions::SemanticVersion;
    use std::str::FromStr;

    #[test]
    fn test_semantic_version() {
        let v1 = SemanticVersion::from_str("1.0").unwrap();
        let v2 = SemanticVersion::from_str("1.0.0").unwrap();
        let v3 = SemanticVersion::from_str("1.1").unwrap();
        let v4 = SemanticVersion::from_str("2.0").unwrap();
        let v5 = SemanticVersion::from_str("2.0.1").unwrap();
        let v6 = SemanticVersion::from_str("2.1.1").unwrap();

        assert!(v1 < v3);
        assert!(v2 <= v3);
        assert!(v3 < v4);
        assert!(v1 == v2);
        assert!(v5 > v4);
        assert!(v6 > v5);
    }
}
