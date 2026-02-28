use crate::agents::agent::{Agent, KrabsAgentBuilder};
use crate::agents::base_agent::BaseAgent;
use crate::config::config::KrabsConfig;
use crate::memory::memory::InMemoryStore;
use crate::permissions::PermissionGuard;
use crate::providers::provider::LlmProvider;
use crate::tools::registry::ToolRegistry;
use crate::tools::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Dispatch multiple sub-agent tasks concurrently and return all results.
///
/// Unlike `delegate` (which is one-shot and sequential), `dispatch` spawns every
/// listed task as an independent Tokio task. All sub-agents run in parallel; the
/// tool blocks until the last one finishes and returns aggregated results.
///
/// Typical use-case: fan out an exploration across several directories at once,
/// then let the calling agent synthesise the findings.
///
/// # JSON schema
/// ```json
/// {
///   "tasks": [
///     { "profile": "explorer", "task": "explore src/components" },
///     { "profile": "explorer", "task": "explore src/api" },
///     { "profile": "planner",  "task": "draft a migration plan for the auth module" }
///   ]
/// }
/// ```
pub struct DispatchTool {
    config: KrabsConfig,
    provider: Arc<dyn LlmProvider>,
    registry: ToolRegistry,
    permissions: PermissionGuard,
}

impl DispatchTool {
    pub fn new(
        config: KrabsConfig,
        provider: Arc<dyn LlmProvider>,
        registry: ToolRegistry,
        permissions: PermissionGuard,
    ) -> Self {
        Self {
            config,
            provider,
            registry,
            permissions,
        }
    }

    fn resolve_profile(name: &str) -> Option<BaseAgent> {
        BaseAgent::all().iter().find(|a| a.name() == name).copied()
    }
}

#[async_trait]
impl Tool for DispatchTool {
    fn name(&self) -> &str {
        "dispatch"
    }

    fn description(&self) -> &str {
        "Dispatch multiple sub-agent tasks concurrently. \
         All tasks start at the same time and run in parallel. \
         Each task can optionally specify which tools the sub-agent is allowed to use — \
         useful when the planner wants to restrict or grant specific capabilities \
         (e.g. give an explorer only read tools, give a builder write access too). \
         If tools is omitted the sub-agent inherits the full tool registry. \
         Returns all results once every task completes."
    }

    fn parameters(&self) -> Value {
        let available_profiles: Vec<&str> = BaseAgent::all().iter().map(|a| a.name()).collect();
        serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 2,
                    "description": "List of tasks to run in parallel.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "profile": {
                                "type": "string",
                                "description": format!(
                                    "Agent profile to use. Available: {}",
                                    available_profiles.join(", ")
                                )
                            },
                            "task": {
                                "type": "string",
                                "description": "The task description for this sub-agent."
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional allow-list of tool names this sub-agent may use. \
                                                Omit to grant access to all tools in the registry."
                            }
                        },
                        "required": ["profile", "task"]
                    }
                }
            },
            "required": ["tasks"]
        })
    }

    async fn call(&self, args: Value) -> Result<ToolResult> {
        let task_list = args["tasks"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("'tasks' must be an array"))?;

        if task_list.is_empty() {
            return Ok(ToolResult {
                content: "dispatch called with empty task list — nothing to do.".into(),
                is_error: false,
            });
        }

        // Validate all entries up front before spawning anything.
        struct TaskSpec {
            profile: BaseAgent,
            profile_name: String,
            task: String,
            /// None = inherit full registry. Some = restrict to these tool names.
            tool_allow_list: Option<Vec<String>>,
        }

        let mut specs: Vec<TaskSpec> = Vec::with_capacity(task_list.len());
        for (i, entry) in task_list.iter().enumerate() {
            let profile_name = entry["profile"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tasks[{}].profile is required", i))?;
            let task = entry["task"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tasks[{}].task is required", i))?;
            let profile = Self::resolve_profile(profile_name).ok_or_else(|| {
                let available: Vec<&str> = BaseAgent::all().iter().map(|a| a.name()).collect();
                anyhow::anyhow!(
                    "tasks[{}]: unknown profile '{}'. Available: {}",
                    i,
                    profile_name,
                    available.join(", ")
                )
            })?;
            let tool_allow_list = entry["tools"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            });
            specs.push(TaskSpec {
                profile,
                profile_name: profile_name.to_string(),
                task: task.to_string(),
                tool_allow_list,
            });
        }

        // Spawn all sub-agents concurrently.
        let mut handles = Vec::with_capacity(specs.len());
        for spec in specs {
            let config = self.config.clone();
            let provider = Arc::clone(&self.provider);
            let full_registry = self.registry.clone();
            let permissions = self.permissions.clone();

            let handle = tokio::spawn(async move {
                // Build a filtered registry if the planner specified an allow-list.
                let registry = if let Some(ref allowed) = spec.tool_allow_list {
                    let mut r = ToolRegistry::new();
                    for name in allowed {
                        if let Some(tool) = full_registry.get(name) {
                            r.register(tool);
                        }
                    }
                    r
                } else {
                    full_registry
                };

                let agent = KrabsAgentBuilder::new(config, provider)
                    .registry(registry)
                    .memory(InMemoryStore::new())
                    .permissions(permissions)
                    .system_prompt(spec.profile.system_prompt())
                    .build();

                let result = Agent::run(agent.as_ref(), &spec.task).await;
                (spec.profile_name, spec.task, result)
            });

            handles.push(handle);
        }

        // Collect results in dispatch order.
        let mut sections: Vec<String> = Vec::with_capacity(handles.len());
        for (idx, handle) in handles.into_iter().enumerate() {
            match handle.await {
                Ok((profile_name, task, Ok(output))) => {
                    sections.push(format!(
                        "### [{idx}] {profile_name} — {task} ({} tool call(s))\n{}",
                        output.tool_calls_made, output.result
                    ));
                }
                Ok((profile_name, task, Err(e))) => {
                    sections.push(format!(
                        "### [{idx}] {profile_name} — {task}\n[ERROR] {e}"
                    ));
                }
                Err(join_err) => {
                    sections.push(format!("### [{idx}] task panicked: {join_err}"));
                }
            }
        }

        Ok(ToolResult {
            content: sections.join("\n\n"),
            is_error: false,
        })
    }
}
