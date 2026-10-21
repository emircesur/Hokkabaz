use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::effect::Effect;
use crate::error::CoreError;

type Key = (TypeId, Option<String>);

struct Entry {
    gen_id: u64,
    // Holds an `Arc<S>` (S may be `dyn Trait`), type-erased.
    value: Arc<dyn Any + Send + Sync>,
}

#[derive(Default)]
struct Inner {
    map: Mutex<HashMap<Key, Entry>>,
    next: std::sync::atomic::AtomicU64,
}

/// Services keyed by `TypeId` + optional name.
#[derive(Clone, Default)]
pub struct ServiceRegistry(Arc<Inner>);

impl std::fmt::Debug for ServiceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServiceRegistry")
    }
}

fn label<S: ?Sized>(name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{}#{n}", type_name::<S>()),
        None => type_name::<S>().to_owned(),
    }
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn provide<S: ?Sized + Send + Sync + 'static>(
        &self,
        name: Option<&str>,
        svc: Arc<S>,
    ) -> Result<Effect, CoreError> {
        let key: Key = (TypeId::of::<S>(), name.map(str::to_owned));
        let mut map = self.0.map.lock().unwrap_or_else(PoisonError::into_inner);
        if map.contains_key(&key) {
            return Err(CoreError::ServiceExists(label::<S>(name)));
        }
        let gen_id = self
            .0
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        map.insert(
            key.clone(),
            Entry {
                gen_id,
                value: Arc::new(svc),
            },
        );
        drop(map);
        let inner = self.0.clone();
        Ok(Effect::new(move || {
            let mut map = inner.map.lock().unwrap_or_else(PoisonError::into_inner);
            // Only remove our own registration (a later provider may have reused the key).
            if map.get(&key).is_some_and(|e| e.gen_id == gen_id) {
                map.remove(&key);
            }
        }))
    }

    pub fn get<S: ?Sized + Send + Sync + 'static>(&self, name: Option<&str>) -> Option<Arc<S>> {
        let key: Key = (TypeId::of::<S>(), name.map(str::to_owned));
        let map = self.0.map.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&key)?.value.downcast_ref::<Arc<S>>().cloned()
    }

    pub fn require<S: ?Sized + Send + Sync + 'static>(
        &self,
        name: Option<&str>,
    ) -> Result<Arc<S>, CoreError> {
        self.get::<S>(name)
            .ok_or_else(|| CoreError::ServiceMissing(label::<S>(name)))
    }
}
