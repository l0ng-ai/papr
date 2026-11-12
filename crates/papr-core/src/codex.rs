use crate::ai::{ChatOutcome, DeltaSink};
use crate::error::{AppError, AppResult};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

fn executable() -> PathBuf {
    if let Some(path) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(if cfg!(windows) { "codex.exe" } else { "codex" }))
            .find(|path| path.is_file())
    }) {
        return path;
    }
    for path in ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"] {
        if std::path::Path::new(path).is_file() {
            return path.into();
        }
    }
    "codex".into()
}

fn command(directory: &std::path::Path, model: &str, system: &str) -> Command {
    let mut cmd = Command::new(executable());
    cmd.args([
        "exec",
        "--ignore-user-config",
        "--ignore-rules",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--json",
        "--color",
        "never",
    ]);
    for config in [
        "forced_login_method=\"chatgpt\"",
        "approval_policy=\"never\"",
        "web_search=\"disabled\"",
        "project_doc_max_bytes=0",
        "features.shell_tool=false",
        "features.unified_exec=false",
        "features.hooks=false",
        "features.apps=false",
        "features.browser_use=false",
        "features.computer_use=false",
        "features.multi_agent=false",
        "features.multi_agent_v2=false",
        "features.skill_search=false",
        "features.code_mode=false",
        "features.code_mode_host=false",
    ] {
        cmd.args(["-c", config]);
    }
    cmd.args([
        "-c",
        &format!(
            "developer_instructions={}",
            serde_json::to_string(system).unwrap()
        ),
    ]);
    if !model.is_empty() {
        cmd.args(["--model", model]);
    }
    cmd.arg("-")
        .current_dir(directory)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_BASE_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd
}

fn message(event: &Value) -> Option<&str> {
    (event["type"] == "item.completed" && event["item"]["type"] == "agent_message")
        .then(|| event["item"]["text"].as_str())
        .flatten()
}

pub async fn stream_chat(
    model: &str,
    system: &str,
    user: &str,
    sink: &mut DeltaSink<'_>,
) -> AppResult<ChatOutcome> {
    let directory = tempfile::tempdir().map_err(|_| AppError::code("codexFailed"))?;
    let child = command(directory.path(), model, system)
        .spawn()
        .map_err(|e| {
            AppError::code(if e.kind() == std::io::ErrorKind::NotFound {
                "codexNotFound"
            } else {
                "codexFailed"
            })
        })?;
    run(child, user, sink, Duration::from_secs(300)).await
}

async fn run(
    mut child: tokio::process::Child,
    user: &str,
    sink: &mut DeltaSink<'_>,
    timeout: Duration,
) -> AppResult<ChatOutcome> {
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let run = async {
        input
            .write_all(user.as_bytes())
            .await
            .map_err(|_| AppError::code("codexFailed"))?;
        input
            .shutdown()
            .await
            .map_err(|_| AppError::code("codexFailed"))?;
        drop(input);
        let mut reader = BufReader::new(output);
        let mut text = String::new();
        let mut completed = false;
        loop {
            let mut line = Vec::new();
            let read = (&mut reader)
                .take(8 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line)
                .await
                .map_err(|_| AppError::code("codexFailed"))?;
            if read == 0 {
                break;
            }
            if line.len() > 8 * 1024 * 1024 {
                return Err(AppError::code("codexFailed"));
            }
            let event: Value =
                serde_json::from_slice(&line).map_err(|_| AppError::code("codexFailed"))?;
            match event["type"].as_str() {
                Some("error" | "turn.failed") => return Err(AppError::code("codexFailed")),
                Some("turn.completed") => completed = true,
                _ => {}
            }
            if let Some(delta) = message(&event) {
                if text.len() + delta.len() > 8 * 1024 * 1024 {
                    return Err(AppError::code("codexFailed"));
                }
                text.push_str(delta);
                if !sink(delta) {
                    return Ok(ChatOutcome {
                        text,
                        completed: false,
                    });
                }
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|_| AppError::code("codexFailed"))?;
        if !status.success() || !completed || text.trim().is_empty() {
            return Err(AppError::code("codexFailed"));
        }
        Ok(ChatOutcome {
            text,
            completed: true,
        })
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| AppError::code("codexTimeout"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_completed_agent_messages_reach_the_reader() {
        assert_eq!(
            message(
                &json!({"type":"item.completed","item":{"type":"agent_message","text":"Summary"}})
            ),
            Some("Summary")
        );
        for kind in ["reasoning", "command_execution", "mcp_tool_call"] {
            assert_eq!(
                message(&json!({"type":"item.completed","item":{"type":kind,"text":"Hidden"}})),
                None
            );
        }
        assert_eq!(
            message(
                &json!({"type":"item.started","item":{"type":"agent_message","text":"Partial"}})
            ),
            None
        );
    }

    #[test]
    fn subscription_does_not_require_an_api_key() {
        let cfg = crate::ai::AiConfig::new(Some("codex".into()), None, None, None).unwrap();
        assert_eq!(cfg.model(), "");
        assert!(crate::ai::AiConfig::new(Some("openai".into()), None, None, None).is_err());
    }

    #[test]
    fn command_keeps_prompts_out_of_shell_and_forces_subscription_auth() {
        let cmd = command(
            std::path::Path::new("/tmp"),
            "test-model",
            "Summarize\nthis",
        );
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"forced_login_method=\"chatgpt\"".into()));
        assert!(args.contains(&"features.shell_tool=false".into()));
        assert!(args.contains(&"--ignore-user-config".into()));
        assert!(args.contains(&"--ephemeral".into()));
        assert!(args.contains(&"test-model".into()));
        assert_eq!(args.last().unwrap(), "-");
    }

    #[cfg(unix)]
    fn fake(script: &str) -> tokio::process::Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_returns_only_final_text_and_requires_successful_turn() {
        let script = "cat >/dev/null; printf '%s\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"hidden\"}}' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Summary\"}}' '{\"type\":\"turn.completed\"}'";
        let mut received = String::new();
        let mut sink = |delta: &str| {
            received.push_str(delta);
            true
        };
        let outcome = run(fake(script), "article", &mut sink, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(outcome.completed);
        assert_eq!(outcome.text, "Summary");
        assert_eq!(received, "Summary");
        for script in [
            "cat >/dev/null; echo '{\"type\":\"turn.failed\"}'",
            "cat >/dev/null; echo '{\"type\":\"turn.completed\"}'; exit 1",
            "cat >/dev/null; echo '{\"type\":\"turn.completed\"}'",
            "cat >/dev/null; echo 'not JSON'",
            "cat >/dev/null; echo '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Partial\"}}'",
        ] {
            assert!(run(fake(script), "article", &mut |_| true, Duration::from_secs(2)).await.is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_sink_is_not_a_completed_response() {
        let script = "cat >/dev/null; echo '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Summary\"}}'";
        let outcome = run(
            fake(script),
            "article",
            &mut |_| false,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert!(!outcome.completed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_process_times_out() {
        let result = run(
            fake("cat >/dev/null; exec sleep 10"),
            "article",
            &mut |_| true,
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(result.err().unwrap().to_string(), "codexTimeout");
    }
}
