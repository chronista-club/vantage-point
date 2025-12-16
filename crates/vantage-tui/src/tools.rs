use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::process::Command;

/// Tool definitions for Claude API
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Tool call from Claude
#[derive(Debug, Clone, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Tool result to send back
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    #[serde(rename = "type")]
    pub result_type: String,
    pub tool_use_id: String,
    pub content: String,
}

impl ToolResult {
    pub fn success(tool_use_id: String, content: String) -> Self {
        Self {
            result_type: "tool_result".to_string(),
            tool_use_id,
            content,
        }
    }

    pub fn error(tool_use_id: String, error: String) -> Self {
        Self {
            result_type: "tool_result".to_string(),
            tool_use_id,
            content: format!("Error: {}", error),
        }
    }
}

/// Get all available tools
pub fn get_tools() -> Vec<Tool> {
    vec![
        // File tools
        Tool {
            name: "read_file".to_string(),
            description: "Read the contents of a file at the specified path".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The absolute path to the file to read"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "write_file".to_string(),
            description: "Write content to a file at the specified path".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The absolute path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        },
        Tool {
            name: "list_dir".to_string(),
            description: "List files and directories in the specified path".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The path to the directory to list"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "run_command".to_string(),
            description: "Run a shell command and return the output".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional working directory for the command"
                    }
                },
                "required": ["command"]
            }),
        },
        Tool {
            name: "search_files".to_string(),
            description: "Search for files matching a pattern using glob".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files (e.g., '**/*.rs')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Base path to search from"
                    }
                },
                "required": ["pattern", "path"]
            }),
        },
        // GitHub tools
        Tool {
            name: "gh_list_issues".to_string(),
            description: "List GitHub issues for the current repository".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "description": "Filter by state: open, closed, all (default: open)"
                    },
                    "milestone": {
                        "type": "string",
                        "description": "Filter by milestone number or title"
                    },
                    "labels": {
                        "type": "string",
                        "description": "Filter by labels (comma-separated)"
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "gh_create_issue".to_string(),
            description: "Create a new GitHub issue".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Issue title"
                    },
                    "body": {
                        "type": "string",
                        "description": "Issue body/description"
                    },
                    "milestone": {
                        "type": "integer",
                        "description": "Milestone number to assign"
                    },
                    "labels": {
                        "type": "string",
                        "description": "Labels to add (comma-separated)"
                    }
                },
                "required": ["title"]
            }),
        },
        Tool {
            name: "gh_list_milestones".to_string(),
            description: "List GitHub milestones for the current repository".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "description": "Filter by state: open, closed, all (default: open)"
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "gh_create_milestone".to_string(),
            description: "Create a new GitHub milestone".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Milestone title"
                    },
                    "description": {
                        "type": "string",
                        "description": "Milestone description"
                    }
                },
                "required": ["title"]
            }),
        },
    ]
}

/// Execute a tool and return the result
pub async fn execute_tool(tool_use: &ToolUse) -> ToolResult {
    match tool_use.name.as_str() {
        "read_file" => execute_read_file(tool_use).await,
        "write_file" => execute_write_file(tool_use).await,
        "list_dir" => execute_list_dir(tool_use).await,
        "run_command" => execute_run_command(tool_use).await,
        "search_files" => execute_search_files(tool_use).await,
        "gh_list_issues" => execute_gh_list_issues(tool_use).await,
        "gh_create_issue" => execute_gh_create_issue(tool_use).await,
        "gh_list_milestones" => execute_gh_list_milestones(tool_use).await,
        "gh_create_milestone" => execute_gh_create_milestone(tool_use).await,
        _ => ToolResult::error(
            tool_use.id.clone(),
            format!("Unknown tool: {}", tool_use.name),
        ),
    }
}

async fn execute_read_file(tool_use: &ToolUse) -> ToolResult {
    let path = tool_use.input.get("path").and_then(|v| v.as_str());

    match path {
        Some(p) => match tokio::fs::read_to_string(p).await {
            Ok(content) => ToolResult::success(tool_use.id.clone(), content),
            Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
        },
        None => ToolResult::error(tool_use.id.clone(), "Missing 'path' parameter".to_string()),
    }
}

async fn execute_write_file(tool_use: &ToolUse) -> ToolResult {
    let path = tool_use.input.get("path").and_then(|v| v.as_str());
    let content = tool_use.input.get("content").and_then(|v| v.as_str());

    match (path, content) {
        (Some(p), Some(c)) => {
            // Create parent directories if needed
            if let Some(parent) = Path::new(p).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::write(p, c).await {
                Ok(_) => ToolResult::success(
                    tool_use.id.clone(),
                    format!("Successfully wrote to {}", p),
                ),
                Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
            }
        }
        _ => ToolResult::error(
            tool_use.id.clone(),
            "Missing 'path' or 'content' parameter".to_string(),
        ),
    }
}

async fn execute_list_dir(tool_use: &ToolUse) -> ToolResult {
    let path = tool_use.input.get("path").and_then(|v| v.as_str());

    match path {
        Some(p) => {
            let mut entries = Vec::new();
            match tokio::fs::read_dir(p).await {
                Ok(mut dir) => {
                    while let Ok(Some(entry)) = dir.next_entry().await {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let file_type = if entry.path().is_dir() { "dir" } else { "file" };
                        entries.push(format!("[{}] {}", file_type, name));
                    }
                    entries.sort();
                    ToolResult::success(tool_use.id.clone(), entries.join("\n"))
                }
                Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
            }
        }
        None => ToolResult::error(tool_use.id.clone(), "Missing 'path' parameter".to_string()),
    }
}

async fn execute_run_command(tool_use: &ToolUse) -> ToolResult {
    let command = tool_use.input.get("command").and_then(|v| v.as_str());
    let working_dir = tool_use.input.get("working_dir").and_then(|v| v.as_str());

    match command {
        Some(cmd) => {
            let mut process = Command::new("sh");
            process.arg("-c").arg(cmd);

            if let Some(dir) = working_dir {
                process.current_dir(dir);
            }

            match process.output().await {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let result = if output.status.success() {
                        stdout.to_string()
                    } else {
                        format!("Exit code: {}\nstdout:\n{}\nstderr:\n{}",
                            output.status.code().unwrap_or(-1),
                            stdout,
                            stderr)
                    };
                    ToolResult::success(tool_use.id.clone(), result)
                }
                Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
            }
        }
        None => ToolResult::error(tool_use.id.clone(), "Missing 'command' parameter".to_string()),
    }
}

async fn execute_search_files(tool_use: &ToolUse) -> ToolResult {
    let pattern = tool_use.input.get("pattern").and_then(|v| v.as_str());
    let path = tool_use.input.get("path").and_then(|v| v.as_str());

    match (pattern, path) {
        (Some(pat), Some(p)) => {
            // Use find command for simplicity
            let cmd = format!("find {} -name '{}' -type f 2>/dev/null | head -50", p, pat);
            let output = Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
                .await;

            match output {
                Ok(out) => {
                    let result = String::from_utf8_lossy(&out.stdout).to_string();
                    ToolResult::success(tool_use.id.clone(), result)
                }
                Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
            }
        }
        _ => ToolResult::error(
            tool_use.id.clone(),
            "Missing 'pattern' or 'path' parameter".to_string(),
        ),
    }
}

// GitHub tools
const REPO: &str = "chronista-club/vantage-point";

async fn execute_gh_list_issues(tool_use: &ToolUse) -> ToolResult {
    let state = tool_use.input.get("state").and_then(|v| v.as_str()).unwrap_or("open");
    let milestone = tool_use.input.get("milestone").and_then(|v| v.as_str());
    let labels = tool_use.input.get("labels").and_then(|v| v.as_str());

    let mut cmd = format!("gh issue list --repo {} --state {}", REPO, state);
    if let Some(m) = milestone {
        cmd.push_str(&format!(" --milestone \"{}\"", m));
    }
    if let Some(l) = labels {
        cmd.push_str(&format!(" --label \"{}\"", l));
    }
    cmd.push_str(" --json number,title,state,milestone,labels --jq '.[] | \"#\\(.number) [\\(.state)] \\(.title)\"'");

    let output = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .await;

    match output {
        Ok(out) => {
            let result = String::from_utf8_lossy(&out.stdout).to_string();
            if result.is_empty() {
                ToolResult::success(tool_use.id.clone(), "No issues found".to_string())
            } else {
                ToolResult::success(tool_use.id.clone(), result)
            }
        }
        Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
    }
}

async fn execute_gh_create_issue(tool_use: &ToolUse) -> ToolResult {
    let title = tool_use.input.get("title").and_then(|v| v.as_str());
    let body = tool_use.input.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let milestone = tool_use.input.get("milestone").and_then(|v| v.as_i64());
    let labels = tool_use.input.get("labels").and_then(|v| v.as_str());

    match title {
        Some(t) => {
            let mut cmd = format!("gh issue create --repo {} --title \"{}\"", REPO, t);
            if !body.is_empty() {
                cmd.push_str(&format!(" --body \"{}\"", body.replace("\"", "\\\"")));
            }
            if let Some(m) = milestone {
                cmd.push_str(&format!(" --milestone {}", m));
            }
            if let Some(l) = labels {
                cmd.push_str(&format!(" --label \"{}\"", l));
            }

            let output = Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
                .await;

            match output {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if out.status.success() {
                        ToolResult::success(tool_use.id.clone(), format!("Issue created: {}", stdout.trim()))
                    } else {
                        ToolResult::error(tool_use.id.clone(), stderr.to_string())
                    }
                }
                Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
            }
        }
        None => ToolResult::error(tool_use.id.clone(), "Missing 'title' parameter".to_string()),
    }
}

async fn execute_gh_list_milestones(tool_use: &ToolUse) -> ToolResult {
    let state = tool_use.input.get("state").and_then(|v| v.as_str()).unwrap_or("open");

    let cmd = format!(
        "gh api repos/{}/milestones --jq '.[] | select(.state == \"{}\") | \"#\\(.number) \\(.title) (\\(.open_issues) open, \\(.closed_issues) closed)\"'",
        REPO, state
    );

    let output = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .await;

    match output {
        Ok(out) => {
            let result = String::from_utf8_lossy(&out.stdout).to_string();
            if result.is_empty() {
                ToolResult::success(tool_use.id.clone(), "No milestones found".to_string())
            } else {
                ToolResult::success(tool_use.id.clone(), result)
            }
        }
        Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
    }
}

async fn execute_gh_create_milestone(tool_use: &ToolUse) -> ToolResult {
    let title = tool_use.input.get("title").and_then(|v| v.as_str());
    let description = tool_use.input.get("description").and_then(|v| v.as_str()).unwrap_or("");

    match title {
        Some(t) => {
            let cmd = format!(
                "gh api repos/{}/milestones --method POST -f title=\"{}\" -f description=\"{}\"",
                REPO, t, description.replace("\"", "\\\"")
            );

            let output = Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
                .await;

            match output {
                Ok(out) => {
                    if out.status.success() {
                        ToolResult::success(tool_use.id.clone(), format!("Milestone '{}' created", t))
                    } else {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        ToolResult::error(tool_use.id.clone(), stderr.to_string())
                    }
                }
                Err(e) => ToolResult::error(tool_use.id.clone(), e.to_string()),
            }
        }
        None => ToolResult::error(tool_use.id.clone(), "Missing 'title' parameter".to_string()),
    }
}
