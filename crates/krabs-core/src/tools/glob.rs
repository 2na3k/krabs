use super::tool::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use walkdir::WalkDir;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files matching a glob pattern."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern to match files" },
                "path": { "type": "string", "description": "Directory to search in (default: current directory)" }
            },
            "required": ["pattern"]
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' argument"))?;
        let base = args["path"].as_str().unwrap_or(".");
        let full_pattern = if pattern.starts_with('/') {
            pattern.to_string()
        } else {
            format!("{}/{}", base, pattern)
        };
        let matches: Vec<String> = glob::glob(&full_pattern)
            .map_err(|e| anyhow::anyhow!("Invalid glob pattern: {}", e))?
            .filter_map(|r| r.ok())
            .map(|p| p.display().to_string())
            .collect();
        if matches.is_empty() {
            Ok(ToolResult::ok("No files matched."))
        } else {
            Ok(ToolResult::ok(matches.join("\n")))
        }
    }
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search for a pattern in file contents."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory or file to search in" },
                "glob": { "type": "string", "description": "File glob filter (e.g. '*.rs')" },
                "case_insensitive": { "type": "boolean", "description": "Case-insensitive search" }
            },
            "required": ["pattern"]
        })
    }
    async fn call(&self, args: serde_json::Value) -> Result<ToolResult> {
        let pattern_str = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' argument"))?;
        let search_path = args["path"].as_str().unwrap_or(".");
        let file_glob = args["glob"].as_str();
        let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);
        let re = if case_insensitive {
            Regex::new(&format!("(?i){}", pattern_str))
        } else {
            Regex::new(pattern_str)
        }
        .map_err(|e| anyhow::anyhow!("Invalid regex: {}", e))?;
        let glob_pattern = file_glob.map(|g| glob::Pattern::new(g).ok());
        let mut results = Vec::new();
        for entry in WalkDir::new(search_path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            if let Some(Some(ref pat)) = glob_pattern {
                let file_name = entry.file_name().to_string_lossy();
                if !pat.matches(&file_name) {
                    continue;
                }
            }
            let path = entry.path();
            if let Ok(content) = std::fs::read_to_string(path) {
                for (line_num, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        results.push(format!("{}:{}: {}", path.display(), line_num + 1, line));
                    }
                }
            }
        }
        if results.is_empty() {
            Ok(ToolResult::ok("No matches found."))
        } else {
            Ok(ToolResult::ok(results.join("\n")))
        }
    }
}
