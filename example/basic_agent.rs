//! Basic agent usage example.
//!
//! Run with:
//!   cargo run --example basic_agent
//!
//! Requires OPENAI_API_KEY (or KRABS_API_KEY) to be set, or a ~/.krabs/config.json.

use krabs_core::{
    agents::{
        agent::{Agent, KrabsAgentBuilder},
        minikrabs::{MiniKrabsSpawner, SpawnMode},
    },
    config::KrabsConfig,
    memory::memory::InMemoryStore,
    permissions::permissions::PermissionGuard,
    prompts::system::SystemPromptBuilder,
    providers::openai::OpenAiProvider,
    tools::{
        bash::BashTool,
        glob::{GlobTool, GrepTool},
        read::ReadTool,
        registry::ToolRegistry,
        write::WriteTool,
    },
};
use std::sync::Arc;

fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashTool));
    registry.register(Arc::new(ReadTool));
    registry.register(Arc::new(WriteTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry
}

fn build_agent(config: KrabsConfig, registry: ToolRegistry) -> Arc<krabs_core::KrabsAgent> {
    let provider = OpenAiProvider::new(&config.base_url, &config.api_key, &config.model);

    let system_prompt = SystemPromptBuilder::new(
        "You are Krabs, a ruthless and efficient coding agent. \
         Complete tasks with minimal resource use. Every action must earn its keep.",
    )
    .with_tools(&registry.tool_defs())
    .build();

    KrabsAgentBuilder::new(config, provider)
        .registry(registry)
        .memory(InMemoryStore::new())
        .permissions(PermissionGuard::new())
        .system_prompt(system_prompt)
        .build()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = KrabsConfig::load()?;

    // --- Example 1: single agent run ---
    println!("=== Single agent ===\n");

    let agent = build_agent(config.clone(), build_registry());
    let task = "List all .rs files in the current directory and summarise what each one does.";
    println!("Task: {task}\n");

    let output = agent.run(task).await?;
    println!("Result:\n{}", output.result);
    println!("\n--- stats ---");
    println!("Tool calls : {}", output.tool_calls_made);
    let (inp, out) = agent.total_tokens();
    println!("Tokens     : {} in / {} out", inp, out);
    println!("Context    : {:.1}%\n", agent.context_used_pct() * 100.0);

    // --- Example 2: parallel sub-agents (MiniKrabs) ---
    println!("=== Parallel sub-agents ===\n");

    let spawner = MiniKrabsSpawner::new(build_agent(config, build_registry()));

    let tasks = vec![
        "Count the number of structs defined in the codebase.".to_string(),
        "List all public traits defined in the codebase.".to_string(),
        "Find any uses of the word TODO in source files.".to_string(),
    ];

    println!("Spawning {} tasks concurrently...\n", tasks.len());
    let results = spawner.spawn_many(tasks.clone(), SpawnMode::Task).await;

    for (task, result) in tasks.iter().zip(results) {
        println!("Task: {task}");
        match result {
            Ok(o) => println!("Result: {}\n", o.result),
            Err(e) => println!("Error: {e}\n"),
        }
    }

    Ok(())
}
