use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::effect::Effect;
use crate::error::CoreError;

type Fut<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type InterceptFn<E> = dyn Fn(E) -> Fut<Verdict<E>> + Send + Sync;
type ListenFn<E> = dyn Fn(Arc<E>) -> Fut<()> + Send + Sync;

/// Domains (prefix before `/` in the event name) where interceptors may run.
pub const INTERCEPTABLE_DOMAINS: &[&str] = &["agent", "tools", "fs"];

/// Typed bus event. `NAME` is `domain/action`.
pub trait Event: Clone + Send + Sync + 'static {
    const NAME: &'static str;
}

/// Interceptor outcome.
#[derive(Debug)]
pub enum Verdict<E> {
    Continue,
    Rewrite(E),
    Reject(String),
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("event `{event}` rejected: {reason}")]
pub struct Rejected {
    pub event: &'static str,
    pub reason: String,
}

struct Entry<H: ?Sized> {
    id: u64,
    priority: i32,
    handler: Arc<H>,
}

struct Slot<E> {
    interceptors: Vec<Entry<InterceptFn<E>>>,
    listeners: Vec<Entry<ListenFn<E>>>,
}

impl<E> Default for Slot<E> {
    fn default() -> Self {
        Self {
            interceptors: Vec::new(),
            listeners: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Inner {
    slots: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
    next: AtomicU64,
}

impl Inner {
    fn with_slot<E: Event, R>(&self, f: impl FnOnce(&mut Slot<E>) -> R) -> R {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots
            .entry(TypeId::of::<E>())
            .or_insert_with(|| Box::new(Slot::<E>::default()))
            .downcast_mut::<Slot<E>>()
            .expect("slot type is keyed by TypeId");
        f(slot)
    }
}

/// Typed event bus. Interceptors (ordered by priority, then registration) run first and may
/// rewrite or reject; listeners then observe the final event. Cheap to clone.
#[derive(Clone, Default)]
pub struct EventBus(Arc<Inner>);

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventBus")
    }
}

fn domain(name: &str) -> &str {
    name.split_once('/').map_or(name, |(d, _)| d)
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe events (post-interception). Listeners run sequentially in registration order.
    pub fn on<E, F, Fut>(&self, f: F) -> Effect
    where
        E: Event,
        F: Fn(Arc<E>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let handler: Arc<ListenFn<E>> = Arc::new(move |e| Box::pin(f(e)));
        self.0.with_slot::<E, _>(|s| {
            s.listeners.push(Entry {
                id,
                priority: 0,
                handler,
            })
        });
        let inner = self.0.clone();
        Effect::new(move || inner.with_slot::<E, _>(|s| s.listeners.retain(|l| l.id != id)))
    }

    /// Intercept events; only for domains in [`INTERCEPTABLE_DOMAINS`]. Lower priority runs first.
    pub fn intercept<E, F, Fut>(&self, priority: i32, f: F) -> Result<Effect, CoreError>
    where
        E: Event,
        F: Fn(E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Verdict<E>> + Send + 'static,
    {
        let d = domain(E::NAME);
        if !INTERCEPTABLE_DOMAINS.contains(&d) {
            return Err(CoreError::InterceptNotAllowed(d.to_owned()));
        }
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let handler: Arc<InterceptFn<E>> = Arc::new(move |e| Box::pin(f(e)));
        self.0.with_slot::<E, _>(|s| {
            s.interceptors.push(Entry {
                id,
                priority,
                handler,
            });
            // Stable sort keeps registration order within a priority.
            s.interceptors.sort_by_key(|i| i.priority);
        });
        let inner = self.0.clone();
        Ok(Effect::new(move || {
            inner.with_slot::<E, _>(|s| s.interceptors.retain(|i| i.id != id))
        }))
    }

    /// Run interceptors, then listeners. Returns the (possibly rewritten) event, or `Rejected`
    /// (listeners are skipped on reject). No lock is held while handlers run.
    pub async fn emit<E: Event>(&self, event: E) -> Result<E, Rejected> {
        let (interceptors, listeners) = self.0.with_slot::<E, _>(|s| {
            (
                s.interceptors
                    .iter()
                    .map(|i| i.handler.clone())
                    .collect::<Vec<_>>(),
                s.listeners
                    .iter()
                    .map(|l| l.handler.clone())
                    .collect::<Vec<_>>(),
            )
        });
        let mut ev = event;
        for h in interceptors {
            match h(ev.clone()).await {
                Verdict::Continue => {}
                Verdict::Rewrite(next) => ev = next,
                Verdict::Reject(reason) => {
                    tracing::debug!(event = E::NAME, %reason, "event rejected by interceptor");
                    return Err(Rejected {
                        event: E::NAME,
                        reason,
                    });
                }
            }
        }
        if !listeners.is_empty() {
            let shared = Arc::new(ev.clone());
            for l in listeners {
                l(shared.clone()).await;
            }
        }
        Ok(ev)
    }
}
