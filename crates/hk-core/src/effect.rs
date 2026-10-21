use std::sync::{Arc, Mutex, PoisonError};

type Disposer = Box<dyn FnOnce() + Send>;

/// Shared, idempotent handle to a registration. Cloneable; disposal runs once.
#[derive(Clone)]
pub struct EffectHandle(Arc<Mutex<Option<Disposer>>>);

impl std::fmt::Debug for EffectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectHandle")
            .field("disposed", &self.is_disposed())
            .finish()
    }
}

impl EffectHandle {
    pub fn dispose(&self) {
        // Run outside the lock: disposers take other locks (bus, services).
        let f = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(f) = f {
            f();
        }
    }

    pub fn is_disposed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    }
}

/// Owning guard: dropping disposes the registration.
#[must_use = "dropping an Effect immediately undoes the registration"]
#[derive(Debug)]
pub struct Effect(EffectHandle);

impl Effect {
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self(EffectHandle(Arc::new(Mutex::new(Some(Box::new(f))))))
    }

    pub fn handle(&self) -> EffectHandle {
        self.0.clone()
    }
}

impl Drop for Effect {
    fn drop(&mut self) {
        self.0.dispose();
    }
}

/// Ordered effect stack. Drop unwinds in reverse registration order.
#[derive(Debug, Default)]
pub struct Scope {
    effects: Vec<Effect>,
}

impl Scope {
    pub fn push(&mut self, e: Effect) {
        self.effects.push(e);
    }

    pub fn len(&self) -> usize {
        self.effects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        while let Some(e) = self.effects.pop() {
            drop(e);
        }
    }
}
