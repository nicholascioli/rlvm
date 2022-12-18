pub mod server;

use std::{os::unix::fs::MetadataExt, path::Path};

use nix::unistd::{Gid, Group, Uid, User};
use serde::Deserialize;
use wax::{Glob, Pattern};

pub mod spec {
    tonic::include_proto!("mountd");
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(deserialize_with = "deserialize_user_from_username")]
    for_user: User,

    #[serde(deserialize_with = "deserialize_group_from_group")]
    for_group: Group,

    /// Whitelist of globs that may be owned by a different user / group pair for mounting
    whitelist: Vec<String>,
}

impl Config {
    /// Check whether a path can be interacted with for the specified config.
    ///
    /// Note: A path is considered interactible iff
    /// - It exists AND
    ///    - It is on the whitelist OR
    ///    - It is owned by the user/group pair
    ///
    /// Furthermore, if the path is supposed to be read-only, then those permissions
    /// are checked as well.
    pub fn ensure_interactable(
        &self,
        path: &Path,
        readonly: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Short out if the path does not exist
        if !path.exists() {
            return Err(format!("path does not exist: {}", path.to_string_lossy()).into());
        }

        let meta = path
            .metadata()
            .map_err(|err| format!("could not get metadata: {}", err.to_string()))?;

        // Optionally check if it needs to be readonly
        if readonly && meta.permissions().readonly() {
            return Err(format!(
                "path is not readonly, but should be: {}",
                path.to_string_lossy()
            )
            .into());
        }

        // Short out if the path is on the whitelist
        let whitelisted = self.whitelist.iter().find(|pattern| {
            let glob = Glob::new(pattern);
            if let Err(e) = glob {
                // TODO: Is there a way that we can bubble this error up instead of silently
                //   ignoring it here?
                log::info!(
                    "invalid glob pattern `{}` in whitelist: {}",
                    pattern,
                    e.to_string()
                );

                return false;
            }

            return glob.unwrap().is_match(path);
        });

        if whitelisted.is_some() {
            return Ok(());
        }

        // Ensure that permissions are respected
        if meta.uid() != self.for_user.uid.as_raw() || meta.gid() != self.for_group.gid.as_raw() {
            return Err(format!(
                "mountpoint is not owned by {}:{} -> {} ({}:{})",
                self.for_user.name,
                self.for_group.name,
                path.to_string_lossy(),
                meta.uid(),
                meta.gid(),
            )
            .into());
        }

        Ok(())
    }

    pub fn get_owner_pair(&self) -> (Uid, Gid) {
        (self.for_user.uid, self.for_group.gid)
    }
}

fn deserialize_user_from_username<'de, D>(deserializer: D) -> Result<User, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    // define a visitor that deserializes
    // `ActualData` encoded as json within a string
    struct UserStringVisitor;

    impl<'de> serde::de::Visitor<'de> for UserStringVisitor {
        type Value = User;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string containing a linux username")
        }

        fn visit_str<E>(self, username: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            User::from_name(username)
                .map_err(|err| {
                    E::custom(format!(
                        "could not get UID from username `{}`: {}",
                        username,
                        err.to_string()
                    ))
                })?
                .ok_or(E::custom(format!(
                    "could not deduce UID from username `{}`: user not found",
                    username,
                )))
        }
    }

    // use our visitor to deserialize an `ActualValue`
    deserializer.deserialize_any(UserStringVisitor)
}

fn deserialize_group_from_group<'de, D>(deserializer: D) -> Result<Group, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    struct GroupStringVisitor;

    impl<'de> serde::de::Visitor<'de> for GroupStringVisitor {
        type Value = Group;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string containing a linux group")
        }

        fn visit_str<E>(self, group: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Group::from_name(group)
                .map_err(|err| {
                    E::custom(format!(
                        "could not get GID from group `{}`: {}",
                        group,
                        err.to_string()
                    ))
                })?
                .ok_or(E::custom(format!(
                    "could not deduce GID from group `{}`: group not found",
                    group,
                )))
        }
    }

    // use our visitor to deserialize an `ActualValue`
    deserializer.deserialize_any(GroupStringVisitor)
}
