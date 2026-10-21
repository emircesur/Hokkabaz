//! Helpers for lossless typed views over JSON objects.
//!
//! Pattern: pull the keys we model out of the object, leave everything else (unknown
//! keys, explicit `null`s, values whose type does not match our model) in `extra`, and
//! merge it back on write. n8n exports are loose about types and add fields between
//! releases, so a derive-based strict model would silently drop data.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Take `key` as `T` if present, non-null and well-typed; otherwise leave it in the map.
pub(crate) fn take<T: DeserializeOwned>(map: &mut Map<String, Value>, key: &str) -> Option<T> {
    let parsed = match map.get(key) {
        None | Some(Value::Null) => return None,
        Some(v) => T::deserialize(v).ok()?,
    };
    map.shift_remove(key);
    Some(parsed)
}

pub(crate) fn take_required<T: DeserializeOwned>(
    map: &mut Map<String, Value>,
    key: &str,
) -> Result<T, String> {
    match map.get(key) {
        None => Err(format!("missing field `{key}`")),
        Some(v) => {
            let parsed = T::deserialize(v).map_err(|e| format!("field `{key}`: {e}"))?;
            map.shift_remove(key);
            Ok(parsed)
        }
    }
}

/// Output builder: typed fields first (stable, n8n-like order), then leftovers.
#[derive(Default)]
pub(crate) struct Out(Map<String, Value>);

impl Out {
    pub(crate) fn put<T: Serialize>(&mut self, key: &str, v: &Option<T>) -> Result<(), String> {
        if let Some(v) = v {
            self.put_req(key, v)?;
        }
        Ok(())
    }

    pub(crate) fn put_req<T: Serialize>(&mut self, key: &str, v: &T) -> Result<(), String> {
        let v = serde_json::to_value(v).map_err(|e| e.to_string())?;
        self.0.insert(key.to_owned(), v);
        Ok(())
    }

    pub(crate) fn finish(mut self, extra: &Map<String, Value>) -> Map<String, Value> {
        for (k, v) in extra {
            if !self.0.contains_key(k) {
                self.0.insert(k.clone(), v.clone());
            }
        }
        self.0
    }
}
