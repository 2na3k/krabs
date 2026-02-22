use anyhow::Result;
use krabs_core::{
    BashTool, Credentials, GlobTool, LlmProvider, Message, ReadTool, StreamChunk, ToolCall,
    ToolDef, ToolRegistry, WriteTool,
};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::io::{self, Write};
use std::sync::Arc;
use tokio::sync::mpsc;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn build_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(BashTool));
    r.register(Arc::new(GlobTool));
    r.register(Arc::new(ReadTool));
    r.register(Arc::new(WriteTool));
    r
}

/// Stream one LLM turn, printing tokens as they arrive.
/// Returns (accumulated_text, tool_calls).
async fn stream_turn(
    provider: &Arc<dyn LlmProvider>,
    messages: &[Message],
    tool_defs: &[ToolDef],
) -> Result<(String, Vec<ToolCall>)> {
    let (tx, mut rx) = mpsc::channel::<Result<StreamChunk, String>>(64);
    let provider_ref = Arc::clone(provider);
    let msgs = messages.to_vec();
    let defs = tool_defs.to_vec();

    tokio::spawn(async move {
        match provider_ref.stream_complete(&msgs, &defs, {
            let (inner_tx, mut inner_rx) = mpsc::channel::<StreamChunk>(64);
            let tx2 = tx.clone();
            tokio::spawn(async move {
                while let Some(chunk) = inner_rx.recv().await {
                    if tx2.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
            });
            inner_tx
        })
        .await
        {
            Ok(()) => {}
            Err(e) => {
                let _ = tx.send(Err(e.to_string())).await;
            }
        }
    });

    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut spinning = true;
    let mut spin_i = 0usize;

    loop {
        tokio::select! {
            item = rx.recv() => {
                match item {
                    None => break,
                    Some(Err(e)) => {
                        if spinning { print!("\r          \r"); }
                        println!("error: {e}");
                        break;
                    }
                    Some(Ok(StreamChunk::Delta { text: t })) => {
                        if spinning {
                            print!("\r          \r");
                            spinning = false;
                        }
                        print!("{t}");
                        io::stdout().flush()?;
                        text.push_str(&t);
                    }
                    Some(Ok(StreamChunk::ToolCallReady { call })) => {
                        if spinning {
                            print!("\r          \r");
                            spinning = false;
                        }
                        tool_calls.push(call);
                    }
                    Some(Ok(StreamChunk::Done { usage })) => {
                        println!(
                            "\n[{} in / {} out tokens]",
                            usage.input_tokens, usage.output_tokens
                        );
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(80)), if spinning => {
                print!("\r{} thinking", SPINNER[spin_i % SPINNER.len()]);
                io::stdout().flush()?;
                spin_i += 1;
            }
        }
    }

    Ok((text, tool_calls))
}

pub async fn run(creds: Credentials) -> Result<()> {
    let provider: Arc<dyn LlmProvider> = Arc::from(creds.build_provider());
    let registry = build_registry();
    let tool_defs = registry.tool_defs();

    let history_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".krabs")
        .join("history");

    let mut editor = DefaultEditor::new()?;
    let _ = editor.load_history(&history_path);

    let mut messages: Vec<Message> = Vec::new();

    loop {
        match editor.readline("Krabs> ") {
            Ok(line) => {
                let input = line.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                editor.add_history_entry(&input)?;

                if input == "/quit" {
                    break;
                }

                messages.push(Message::user(&input));

                // Agentic loop: keep going until no more tool calls
                loop {
                    let (text, calls): (String, Vec<ToolCall>) =
                        stream_turn(&provider, &messages, &tool_defs).await?;

                    if calls.is_empty() {
                        // Final text response
                        println!();
                        if !text.is_empty() {
                            messages.push(Message::assistant(&text));
                        }
                        break;
                    }

                    // Model wants to call tools
                    if !text.is_empty() {
                        println!();
                        messages.push(Message::assistant(&text));
                    }

                    for call in calls {
                        println!("[tool: {} {}]", call.name, call.args);
                        let result = match registry.get(&call.name) {
                            Some(tool) => tool.call(call.args.clone()).await?,
                            None => krabs_core::ToolResult::err(
                                format!("tool '{}' not found", call.name),
                            ),
                        };
                        println!("[result: {}]", result.content);
                        messages.push(Message::tool_result(&result.content, &call.id));
                    }
                }
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }

    let _ = editor.save_history(&history_path);
    Ok(())
}
