use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::bus::{Event, EventBus, Rejected, Verdict};
use crate::effect::{Effect, EffectHandle, Scope};
use crate::error::CoreError;
use crate::services::ServiceRegistry;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginId(u64);

/// A unit mounted into the host. `apply` registers services/handlers through `ctx`.
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<(), CoreError>>;
}

struct Node {
    name: String,
    parent: Option<PluginId>,
    children: Vec<PluginId>,
    scope: Scope,
}

struct HostInner {
    bus: EventBus,
    services: ServiceRegistry,
    nodes: Mutex<HashMap<PluginId, Node>>,
    next: AtomicU64,
}

impl HostInner {
    fn nodes(&self) -> MutexGuard<'_, HashMap<PluginId, Node>> {
        self.nodes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Plugin tree + shared bus and service registry. Cheap to clone.
#[derive(Clone)]
pub struct Host(Arc<HostInner>);

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("plugins", &self.0.nodes().len())
            .finish()
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    pub fn new() -> Self {
        Self(Arc::new(HostInner {
            bus: EventBus::new(),
            services: ServiceRegistry::new(),
            nodes: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }))
    }

    pub fn bus(&self) -> &EventBus {
        &self.0.bus
    }

    pub fn services(&self) -> &ServiceRegistry {
        &self.0.services
    }

    pub async fn emit<E: Event>(&self, event: E) -> Result<E, Rejected> {
        self.0.bus.emit(event).await
    }

    /// Mount a root plugin. On `apply` failure everything it registered is unwound.
    pub async fn load(&self, plugin: Arc<dyn Plugin>) -> Result<PluginId, CoreError> {
        self.load_inner(plugin, None).await
    }

    /// Unload a plugin: children first (reverse load order), then its own effects in reverse.
    pub fn unload(&self, id: PluginId) -> Result<(), CoreError> {
        self.unload_inner(id)
    }

    /// Unload all root plugins, newest first.
    pub fn shutdown(&self) {
        let mut roots: Vec<PluginId> = self
            .0
            .nodes()
            .iter()
            .filter(|(_, n)| n.parent.is_none())
            .map(|(id, _)| *id)
            .collect();
        roots.sort_by_key(|id| id.0);
        for id in roots.into_iter().rev() {
            let _ = self.unload_inner(id);
        }
    }

    pub fn is_loaded(&self, id: PluginId) -> bool {
        self.0.nodes().contains_key(&id)
    }

    pub fn plugin_name(&self, id: PluginId) -> Option<String> {
        self.0.nodes().get(&id).map(|n| n.name.clone())
    }

    async fn load_inner(
        &self,
        plugin: Arc<dyn Plugin>,
        parent: Option<PluginId>,
    ) -> Result<PluginId, CoreError> {
        let id = PluginId(self.0.next.fetch_add(1, Ordering::Relaxed));
        let name = plugin.name().to_owned();
        {
            let mut nodes = self.0.nodes();
            if let Some(p) = parent {
                nodes
                    .get_mut(&p)
                    .ok_or(CoreError::PluginGone)?
                    .children
                    .push(id);
            }
            nodes.insert(
                id,
                Node {
                    name: name.clone(),
                    parent,
                    children: Vec::new(),
                    scope: Scope::default(),
                },
            );
        }
        let ctx = Ctx {
            host: self.clone(),
            id,
        };
        match plugin.apply(&ctx).await {
            Ok(()) => {
                tracing::debug!(plugin = %name, "plugin loaded");
                Ok(id)
            }
            Err(e) => {
                let _ = self.unload_inner(id);
                Err(CoreError::PluginLoad(name, e.to_string()))
            }
        }
    }

    fn unload_inner(&self, id: PluginId) -> Result<(), CoreError> {
        // Remove under the lock, dispose outside it: disposers and child unloads re-enter.
        let node = {
            let mut nodes = self.0.nodes();
            let node = nodes.remove(&id).ok_or(CoreError::PluginGone)?;
            if let Some(p) = node.parent.and_then(|p| nodes.get_mut(&p)) {
                p.children.retain(|c| *c != id);
            }
            node
        };
        for child in node.children.iter().rev() {
            let _ = self.unload_inner(*child);
        }
        tracing::debug!(plugin = %node.name, effects = node.scope.len(), "plugin unloading");
        drop(node.scope);
        Ok(())
    }
}

/// Plugin-scoped view of the host. Registrations are owned by the plugin's scope and
/// disposed on unload; the returned [`EffectHandle`] allows early manual disposal.
#[derive(Clone)]
pub struct Ctx {
    host: Host,
    id: PluginId,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx").field("plugin", &self.id).finish()
    }
}

impl Ctx {
    pub fn plugin_id(&self) -> PluginId {
        self.id
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    fn adopt(&self, effect: Effect) -> Result<EffectHandle, CoreError> {
        let handle = effect.handle();
        match self.host.0.nodes().get_mut(&self.id) {
            Some(n) => {
                n.scope.push(effect);
                Ok(handle)
            }
            // `effect` drops here, undoing the registration.
            None => Err(CoreError::PluginGone),
        }
    }

    pub fn provide<S: ?Sized + Send + Sync + 'static>(
        &self,
        svc: Arc<S>,
    ) -> Result<EffectHandle, CoreError> {
        self.adopt(self.host.0.services.provide(None, svc)?)
    }

    pub fn provide_named<S: ?Sized + Send + Sync + 'static>(
        &self,
        name: &str,
        svc: Arc<S>,
    ) -> Result<EffectHandle, CoreError> {
        self.adopt(self.host.0.services.provide(Some(name), svc)?)
    }

    /// Resolve a service (`ctx.use::<S>()` in the design; `use` is a Rust keyword).
    pub fn use_service<S: ?Sized + Send + Sync + 'static>(&self) -> Result<Arc<S>, CoreError> {
        self.host.0.services.require(None)
    }

    pub fn use_named<S: ?Sized + Send + Sync + 'static>(
        &self,
        name: &str,
    ) -> Result<Arc<S>, CoreError> {
        self.host.0.services.require(Some(name))
    }

    pub fn on<E, F, Fut>(&self, f: F) -> Result<EffectHandle, CoreError>
    where
        E: Event,
        F: Fn(Arc<E>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.adopt(self.host.0.bus.on::<E, _, _>(f))
    }

    pub fn intercept<E, F, Fut>(&self, priority: i32, f: F) -> Result<EffectHandle, CoreError>
    where
        E: Event,
        F: Fn(E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Verdict<E>> + Send + 'static,
    {
        self.adopt(self.host.0.bus.intercept::<E, _, _>(priority, f)?)
    }

    /// Register arbitrary cleanup, run in reverse order with the other effects.
    pub fn on_dispose(&self, f: impl FnOnce() + Send + 'static) -> Result<EffectHandle, CoreError> {
        self.adopt(Effect::new(f))
    }

    pub async fn emit<E: Event>(&self, event: E) -> Result<E, Rejected> {
        self.host.emit(event).await
    }

    /// Mount a child plugin; unloaded together with (before) this plugin.
    pub async fn load_child(&self, plugin: Arc<dyn Plugin>) -> Result<PluginId, CoreError> {
        self.host.load_inner(plugin, Some(self.id)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Clone, Debug)]
    struct ToolCall(String);
    impl Event for ToolCall {
        const NAME: &'static str = "tools/pre-execute";
    }

    #[derive(Clone, Debug)]
    struct Ping;
    impl Event for Ping {
        const NAME: &'static str = "misc/ping";
    }

    trait Greeter: Send + Sync {
        fn hi(&self) -> &'static str;
    }
    struct En;
    impl Greeter for En {
        fn hi(&self) -> &'static str {
            "hi"
        }
    }

    type Applier =
        Box<dyn for<'a> Fn(&'a Ctx) -> BoxFuture<'a, Result<(), CoreError>> + Send + Sync>;
    struct Fn_(&'static str, Applier);
    impl Plugin for Fn_ {
        fn name(&self) -> &str {
            self.0
        }
        fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<(), CoreError>> {
            (self.1)(ctx)
        }
    }
    fn plugin<F>(name: &'static str, f: F) -> Arc<dyn Plugin>
    where
        F: for<'a> Fn(&'a Ctx) -> BoxFuture<'a, Result<(), CoreError>> + Send + Sync + 'static,
    {
        Arc::new(Fn_(name, Box::new(f)))
    }

    #[tokio::test]
    async fn unload_unwinds_effects_in_reverse() {
        let host = Host::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        let o = order.clone();
        let id = host
            .load(plugin("p", move |ctx| {
                let o = o.clone();
                Box::pin(async move {
                    ctx.provide::<dyn Greeter>(Arc::new(En))?;
                    ctx.provide_named("n", Arc::new(5u32))?;
                    let (a, b) = (o.clone(), o.clone());
                    ctx.on_dispose(move || a.lock().unwrap().push(1))?;
                    ctx.on_dispose(move || b.lock().unwrap().push(2))?;
                    ctx.on::<Ping, _, _>(|_| async {})?;
                    Ok(())
                })
            }))
            .await
            .unwrap();
        assert_eq!(host.services().get::<dyn Greeter>(None).unwrap().hi(), "hi");
        assert_eq!(*host.services().get::<u32>(Some("n")).unwrap(), 5);
        host.unload(id).unwrap();
        assert_eq!(*order.lock().unwrap(), vec![2, 1]);
        assert!(host.services().get::<dyn Greeter>(None).is_none());
        assert!(host.services().get::<u32>(Some("n")).is_none());
        assert!(!host.is_loaded(id));
        assert!(matches!(host.unload(id), Err(CoreError::PluginGone)));
    }

    #[tokio::test]
    async fn unloaded_listener_no_longer_fires() {
        let host = Host::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let id = host
            .load(plugin("l", move |ctx| {
                let h = h.clone();
                Box::pin(async move {
                    ctx.on::<Ping, _, _>(move |_| {
                        let h = h.clone();
                        async move {
                            h.fetch_add(1, Ordering::SeqCst);
                        }
                    })?;
                    Ok(())
                })
            }))
            .await
            .unwrap();
        host.emit(Ping).await.unwrap();
        host.unload(id).unwrap();
        host.emit(Ping).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn interceptor_rewrite_and_reject() {
        let host = Host::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
        let id = host
            .load(plugin("guard", move |ctx| {
                let s = s.clone();
                Box::pin(async move {
                    // Runs second (priority 10): rejects "rm".
                    ctx.intercept::<ToolCall, _, _>(10, |e| async move {
                        if e.0 == "rm" {
                            Verdict::Reject("denied".into())
                        } else {
                            Verdict::Continue
                        }
                    })?;
                    // Runs first (priority 0): rewrites "sh" -> "rm".
                    ctx.intercept::<ToolCall, _, _>(0, |e| async move {
                        if e.0 == "sh" {
                            Verdict::Rewrite(ToolCall("rm".into()))
                        } else {
                            Verdict::Continue
                        }
                    })?;
                    ctx.on::<ToolCall, _, _>(move |e| {
                        let s = s.clone();
                        async move { s.lock().unwrap().push(e.0.clone()) }
                    })?;
                    Ok(())
                })
            }))
            .await
            .unwrap();

        let ok = host.emit(ToolCall("ls".into())).await.unwrap();
        assert_eq!(ok.0, "ls");
        let err = host.emit(ToolCall("sh".into())).await.unwrap_err();
        assert_eq!(err.reason, "denied");
        // Rejected events never reach listeners.
        assert_eq!(*seen.lock().unwrap(), vec!["ls".to_string()]);

        host.unload(id).unwrap();
        assert_eq!(host.emit(ToolCall("rm".into())).await.unwrap().0, "rm");
    }

    #[tokio::test]
    async fn rewrite_reaches_listeners() {
        let host = Host::new();
        let bus = host.bus();
        let _i = bus
            .intercept::<ToolCall, _, _>(0, |_| async { Verdict::Rewrite(ToolCall("new".into())) })
            .unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let s = seen.clone();
        let _l = bus.on::<ToolCall, _, _>(move |e| {
            let s = s.clone();
            async move { *s.lock().unwrap() = e.0.clone() }
        });
        assert_eq!(host.emit(ToolCall("old".into())).await.unwrap().0, "new");
        assert_eq!(*seen.lock().unwrap(), "new");
    }

    #[tokio::test]
    async fn intercept_disallowed_domain() {
        let host = Host::new();
        let r = host
            .bus()
            .intercept::<Ping, _, _>(0, |_| async { Verdict::Continue });
        assert!(matches!(r, Err(CoreError::InterceptNotAllowed(d)) if d == "misc"));
    }

    #[tokio::test]
    async fn duplicate_service_rejected_and_failed_load_unwinds() {
        let host = Host::new();
        host.load(plugin("a", |ctx| {
            Box::pin(async move {
                ctx.provide(Arc::new(1u8))?;
                Ok(())
            })
        }))
        .await
        .unwrap();
        let err = host
            .load(plugin("b", |ctx| {
                Box::pin(async move {
                    ctx.provide_named("x", Arc::new(2u8))?;
                    ctx.provide(Arc::new(3u8))?; // dup -> fail
                    Ok(())
                })
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::PluginLoad(..)));
        // b's earlier registration was unwound; a's survives.
        assert!(host.services().get::<u8>(Some("x")).is_none());
        assert_eq!(*host.services().get::<u8>(None).unwrap(), 1);
    }

    #[tokio::test]
    async fn child_unloads_before_parent_and_with_it() {
        let host = Host::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        let o = order.clone();
        let id = host
            .load(plugin("parent", move |ctx| {
                let o = o.clone();
                Box::pin(async move {
                    let po = o.clone();
                    ctx.on_dispose(move || po.lock().unwrap().push("parent"))?;
                    let co = o.clone();
                    ctx.load_child(plugin("child", move |c| {
                        let co = co.clone();
                        Box::pin(async move {
                            c.on_dispose(move || co.lock().unwrap().push("child"))?;
                            Ok(())
                        })
                    }))
                    .await?;
                    Ok(())
                })
            }))
            .await
            .unwrap();
        host.unload(id).unwrap();
        assert_eq!(*order.lock().unwrap(), vec!["child", "parent"]);
    }

    #[tokio::test]
    async fn ctx_after_unload_errors() {
        let host = Host::new();
        let slot = Arc::new(Mutex::new(None::<Ctx>));
        let s = slot.clone();
        let id = host
            .load(plugin("p", move |ctx| {
                let s = s.clone();
                Box::pin(async move {
                    *s.lock().unwrap() = Some(ctx.clone());
                    Ok(())
                })
            }))
            .await
            .unwrap();
        host.unload(id).unwrap();
        let ctx = slot.lock().unwrap().take().unwrap();
        assert!(matches!(
            ctx.provide(Arc::new(1i64)),
            Err(CoreError::PluginGone)
        ));
        // Rolled back: not left registered.
        assert!(host.services().get::<i64>(None).is_none());
    }
}
