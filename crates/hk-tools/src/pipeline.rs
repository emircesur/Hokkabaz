use hk_core::EventBus;
use serde_json::Value;

use crate::events::{ToolExecute, ToolPostExecute, ToolPreExecute};
use crate::output::{OutputLimits, truncate_and_spill};
use crate::registry::ToolRegistry;
use crate::tool::{ToolCtx, ToolError, ToolOutput};

/// Guarded execution: validate -> `tools/pre-execute` -> revalidate if rewritten ->
/// `tools/execute` -> run -> `tools/post-execute` (redaction) -> truncate/spill.
#[derive(Debug, Clone)]
pub struct ToolPipeline {
    registry: ToolRegistry,
    bus: EventBus,
    limits: OutputLimits,
}

impl ToolPipeline {
    pub fn new(registry: ToolRegistry, bus: EventBus, limits: OutputLimits) -> Self {
        Self {
            registry,
            bus,
            limits,
        }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Err means the call never produced a tool result (unknown tool, bad args, rejected, tool
    /// failure); callers turn it into an error result via [`ToolError::to_output`].
    pub async fn run(
        &self,
        ctx: ToolCtx,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .registry
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_owned()))?;
        self.registry.validate(name, &args)?;

        let pre = self
            .bus
            .emit(ToolPreExecute {
                call_id: ctx.call_id.clone(),
                tool: name.to_owned(),
                risk: tool.risk(&args),
                args: args.clone(),
            })
            .await
            .map_err(|r| ToolError::Rejected {
                tool: name.to_owned(),
                reason: r.reason,
            })?;
        let args = pre.args;
        // An interceptor may have rewritten args; they must still satisfy the schema.
        self.registry.validate(name, &args)?;

        let exec = self
            .bus
            .emit(ToolExecute {
                call_id: ctx.call_id.clone(),
                tool: name.to_owned(),
                args,
            })
            .await
            .map_err(|r| ToolError::Rejected {
                tool: name.to_owned(),
                reason: r.reason,
            })?;
        self.registry.validate(name, &exec.args)?;

        let call_id = ctx.call_id.clone();
        let raw = tool.execute(ctx, exec.args).await?;

        let post = self
            .bus
            .emit(ToolPostExecute {
                call_id: call_id.clone(),
                tool: name.to_owned(),
                output: raw,
            })
            .await
            .map_err(|r| ToolError::Rejected {
                tool: name.to_owned(),
                reason: r.reason,
            })?;
        let mut out = post.output;
        if let Some(s) = truncate_and_spill(&call_id, &out.content, &self.limits).await {
            out.content = s.content;
            out.spill_path = s.path;
            out.original_bytes = Some(s.original_bytes);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use hk_core::Verdict;
    use serde_json::{Value, json};

    use super::*;
    use crate::tool::{Risk, Tool, ToolSpec};

    struct Echo {
        calls: Arc<AtomicUsize>,
        out: String,
    }

    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".into(),
                description: "echo".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                    "additionalProperties": false
                }),
            }
        }
        fn risk(&self, _: &Value) -> Risk {
            Risk::Ask
        }
        async fn execute(&self, _: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let t = args["text"].as_str().unwrap_or_default();
            Ok(ToolOutput::ok(format!("{}{t}", self.out)))
        }
    }

    fn setup(
        out: &str,
        max: usize,
        dir: &std::path::Path,
    ) -> (ToolPipeline, EventBus, Arc<AtomicUsize>) {
        let reg = ToolRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        std::mem::forget(
            reg.register(Arc::new(Echo {
                calls: calls.clone(),
                out: out.into(),
            }))
            .unwrap(),
        );
        let bus = EventBus::new();
        let mut limits = OutputLimits::new(dir);
        limits.max_bytes = max;
        (ToolPipeline::new(reg, bus.clone(), limits), bus, calls)
    }

    fn ctx(id: &str) -> ToolCtx {
        ToolCtx {
            call_id: id.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn happy_path() {
        let d = tempfile::tempdir().unwrap();
        let (p, _, calls) = setup("", 1000, d.path());
        let o = p
            .run(ctx("c1"), "echo", json!({"text": "hi"}))
            .await
            .unwrap();
        assert_eq!(o.content, "hi");
        assert!(o.spill_path.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_tool_and_bad_args_never_execute() {
        let d = tempfile::tempdir().unwrap();
        let (p, _, calls) = setup("", 1000, d.path());
        assert!(matches!(
            p.run(ctx("c"), "nope", json!({})).await,
            Err(ToolError::Unknown(_))
        ));
        for bad in [
            json!({}),
            json!({"text": 3}),
            json!({"text": "x", "extra": 1}),
            json!("s"),
        ] {
            let e = p.run(ctx("c"), "echo", bad).await.unwrap_err();
            assert!(matches!(e, ToolError::InvalidArgs { .. }), "{e}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pre_execute_reject_blocks_tool() {
        let d = tempfile::tempdir().unwrap();
        let (p, bus, calls) = setup("", 1000, d.path());
        let _g = bus
            .intercept::<ToolPreExecute, _, _>(0, |e| async move {
                assert_eq!(e.risk, Risk::Ask);
                Verdict::Reject("policy: denied".into())
            })
            .unwrap();
        let e = p
            .run(ctx("c"), "echo", json!({"text": "hi"}))
            .await
            .unwrap_err();
        assert!(matches!(&e, ToolError::Rejected { reason, .. } if reason == "policy: denied"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rewritten_args_are_revalidated() {
        let d = tempfile::tempdir().unwrap();
        let (p, bus, calls) = setup("", 1000, d.path());
        let g = bus
            .intercept::<ToolPreExecute, _, _>(0, |mut e| async move {
                e.args = json!({"text": 42});
                Verdict::Rewrite(e)
            })
            .unwrap();
        let e = p
            .run(ctx("c"), "echo", json!({"text": "hi"}))
            .await
            .unwrap_err();
        assert!(matches!(e, ToolError::InvalidArgs { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(g);
        let _ok = bus
            .intercept::<ToolPreExecute, _, _>(0, |mut e| async move {
                e.args = json!({"text": "rewritten"});
                Verdict::Rewrite(e)
            })
            .unwrap();
        let o = p
            .run(ctx("c"), "echo", json!({"text": "hi"}))
            .await
            .unwrap();
        assert_eq!(o.content, "rewritten");
    }

    #[tokio::test]
    async fn post_execute_redacts_before_spill() {
        let d = tempfile::tempdir().unwrap();
        let (p, bus, _) = setup("", 20, d.path());
        let _g = bus
            .intercept::<ToolPostExecute, _, _>(0, |mut e| async move {
                e.output.content = e.output.content.replace("SECRET", "[redacted]");
                Verdict::Rewrite(e)
            })
            .unwrap();
        let text = format!("{}SECRET{}", "a".repeat(100), "b".repeat(100));
        let o = p
            .run(ctx("r1"), "echo", json!({"text": text}))
            .await
            .unwrap();
        let spilled = std::fs::read_to_string(o.spill_path.unwrap()).unwrap();
        assert!(!spilled.contains("SECRET") && spilled.contains("[redacted]"));
        assert!(!o.content.contains("SECRET"));
    }

    #[tokio::test]
    async fn large_output_spills_and_truncates() {
        let d = tempfile::tempdir().unwrap();
        let (p, _, _) = setup("", 100, d.path());
        let text = "é".repeat(500); // multibyte: cut must land on char boundaries
        let o = p
            .run(ctx("../evil/id"), "echo", json!({"text": text.clone()}))
            .await
            .unwrap();
        let path = o.spill_path.clone().expect("spilled");
        assert_eq!(path.parent().unwrap(), d.path());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(o.original_bytes, Some(text.len()));
        assert!(o.content.len() < 200);
        assert!(o.content.contains("bytes omitted"));
        assert!(o.content.contains(path.to_str().unwrap()));
    }

    #[tokio::test]
    async fn spill_failure_still_truncates() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let (p, _, _) = setup("", 50, &file);
        let o = p
            .run(ctx("c"), "echo", json!({"text": "z".repeat(1000)}))
            .await
            .unwrap();
        assert!(o.spill_path.is_none());
        assert!(o.content.len() < 200);
    }

    #[tokio::test]
    async fn registry_dedup_bad_schema_and_unregister() {
        struct Bad;
        #[async_trait]
        impl Tool for Bad {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "bad".into(),
                    description: String::new(),
                    parameters: json!({"type": 12}),
                }
            }
            async fn execute(&self, _: ToolCtx, _: Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::ok(""))
            }
        }
        let d = tempfile::tempdir().unwrap();
        let (p, _, calls) = setup("", 10, d.path());
        let reg = p.registry();
        assert!(matches!(
            reg.register(Arc::new(Echo {
                calls,
                out: String::new()
            })),
            Err(ToolError::Duplicate(_))
        ));
        assert!(matches!(
            reg.register(Arc::new(Bad)),
            Err(ToolError::BadSchema { .. })
        ));
        let fx = reg.register(Arc::new(Bad2)).unwrap();
        assert_eq!(reg.names(), ["bad2", "echo"]);
        drop(fx);
        assert_eq!(reg.names(), ["echo"]);

        struct Bad2;
        #[async_trait]
        impl Tool for Bad2 {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "bad2".into(),
                    description: String::new(),
                    parameters: json!({"type": "object"}),
                }
            }
            async fn execute(&self, _: ToolCtx, _: Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::ok(""))
            }
        }
    }
}
