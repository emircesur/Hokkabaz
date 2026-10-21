//! Node `typeVersion` handling. n8n versions are decimals (`1`, `1.1`, `2.2`, `4.2`).

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Totally ordered node version. Never NaN (rejected on construction), so `Eq`/`Hash` are sound.
#[derive(Clone, Copy)]
pub struct Version(f64);

impl Version {
    pub fn new(v: f64) -> Option<Self> {
        v.is_finite().then_some(Self(v + 0.0)) // `+ 0.0` folds -0.0 into 0.0 for hashing
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for Version {}
impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}
impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}
impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0) // f64 Display prints `1` for 1.0, `1.1` for 1.1
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.0.fract() == 0.0 && self.0.abs() < 1e15 {
            s.serialize_i64(self.0 as i64)
        } else {
            s.serialize_f64(self.0)
        }
    }
}
impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = f64::deserialize(d)?;
        Version::new(v).ok_or_else(|| serde::de::Error::custom("non-finite version"))
    }
}

/// `(type, typeVersion)` — identity of a node implementation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey {
    pub node_type: String,
    pub version: Version,
}

impl NodeKey {
    pub fn new(node_type: impl Into<String>, version: Version) -> Self {
        Self {
            node_type: node_type.into(),
            version,
        }
    }
}

impl fmt::Display for NodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.node_type, self.version)
    }
}

/// Description `version` field: a single number or a list of supported versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NodeVersions {
    Single(Version),
    Many(Vec<Version>),
}

impl NodeVersions {
    pub fn to_vec(&self) -> Vec<Version> {
        match self {
            Self::Single(v) => vec![*v],
            Self::Many(vs) => vs.clone(),
        }
    }

    pub fn contains(&self, v: Version) -> bool {
        match self {
            Self::Single(s) => *s == v,
            Self::Many(vs) => vs.contains(&v),
        }
    }

    pub fn latest(&self) -> Option<Version> {
        self.to_vec().into_iter().max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_versions_serialize_as_integers() {
        let v = Version::new(2.0).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), "2");
        let v = Version::new(4.2).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), "4.2");
    }

    #[test]
    fn versions_list_or_single() {
        let s: NodeVersions = serde_json::from_str("[1, 1.1, 2]").unwrap();
        assert_eq!(s.latest().unwrap().get(), 2.0);
        let s: NodeVersions = serde_json::from_str("3.1").unwrap();
        assert!(s.contains(Version::new(3.1).unwrap()));
    }
}
