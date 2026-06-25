//! `papr setup` — register an ambient SessionStart integration so every agent
//! conversation starts with the current unread state already in context (AXI
//! §7). Supports Claude Code, Codex and OpenCode; installs are idempotent and
//! repair a stale binary path on re-run.

use crate::{AxiError, Doc};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Which agent host(s) to wire up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum App {
    Claude,
    Codex,
    OpenCode,
}

impl App {
    fn parse(s: &str) -> Option<Vec<App>> {
        match s {
            "all" => Some(vec![App::Claude, App::Codex, App::OpenCode]),
            "claude" => Some(vec![App::Claude]),
            "codex" => Some(vec![App::Codex]),
            "opencode" => Some(vec![App::OpenCode]),
            _ => None,
        }
    }
}

pub fn run(app: &str) -> Result<String, AxiError> {
    let apps = App::parse(app).ok_or_else(|| {
        AxiError::usage(
            format!("unknown app target `{app}`"),
            vec!["Run `papr setup --app all|claude|codex|opencode`".into()],
        )
    })?;
    let bin = resolve_bin();

    let mut apps_rows: Vec<Value> = Vec::new();
    for a in apps {
        let (name, result) = match a {
            App::Claude => ("claude", install_claude(&bin)),
            App::Codex => ("codex", install_codex(&bin)),
            App::OpenCode => ("opencode", install_opencode(&bin)),
        };
        let row = match result {
            Ok(status) => json!({ "app": name, "status": "ok", "detail": status }),
            Err(e) => json!({ "app": name, "status": "error", "detail": e }),
        };
        apps_rows.push(row);
    }

    let mut d = Doc::new();
    d.set("setup", json!({ "bin": bin }));
    d.set("apps", Value::Array(apps_rows));
    d.help(vec![
        "Start a new agent session — the unread dashboard loads automatically".into(),
        "Run `papr` to preview the context that will be injected".into(),
    ]);
    Ok(d.into_toon())
}

/// The command an integration should invoke. Prefer the bare name `papr` when it
/// is on PATH and resolves to *this* executable (keeps a global install
/// portable); otherwise fall back to the absolute path.
fn resolve_bin() -> String {
    let current = std::env::current_exe().ok();
    if let (Some(cur), Ok(path)) = (current.as_ref(), std::env::var("PATH")) {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("papr");
            if candidate.is_file() {
                // Compare canonical paths so a symlink to this binary still counts.
                if let (Ok(a), Ok(b)) = (candidate.canonicalize(), cur.canonicalize()) {
                    if a == b {
                        return "papr".to_string();
                    }
                }
            }
        }
    }
    current
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "papr".to_string())
}

fn home() -> Result<PathBuf, AxiError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| AxiError::runtime("could not resolve HOME"))
}

// ───────────────────────────── Claude Code ─────────────────────────────

/// Merge a SessionStart command hook into `~/.claude/settings.json`, preserving
/// every other key. Re-running repairs the binary path; a matching hook is a
/// silent no-op.
fn install_claude(bin: &str) -> Result<String, String> {
    let dir = home().map_err(|e| e.message.clone())?.join(".claude");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir .claude: {e}"))?;
    let file = dir.join("settings.json");

    let mut root: serde_json::Value = if file.exists() {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("read settings.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("settings.json is not valid JSON: {e}"))?
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        return Err("settings.json root is not an object".into());
    }
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let sessions = hooks
        .as_object_mut()
        .ok_or("hooks is not an object")?
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]));
    let arr = sessions.as_array_mut().ok_or("SessionStart is not an array")?;

    // Find an existing papr hook (command basename == "papr") to repair in place.
    let is_papr = |cmd: &str| cmd == "papr" || cmd.ends_with("/papr");
    let changed;
    let mut found = false;
    for group in arr.iter_mut() {
        if let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            for h in inner.iter_mut() {
                if let Some(cmd) = h.get("command").and_then(|c| c.as_str()) {
                    if is_papr(cmd) {
                        found = true;
                        if cmd != bin {
                            h["command"] = serde_json::json!(bin);
                        }
                    }
                }
            }
        }
    }
    if found {
        changed = "updated";
    } else {
        arr.push(serde_json::json!({
            "hooks": [ { "type": "command", "command": bin } ]
        }));
        changed = "installed";
    }

    let pretty =
        serde_json::to_string_pretty(&root).map_err(|e| format!("serialize settings.json: {e}"))?;
    std::fs::write(&file, pretty + "\n").map_err(|e| format!("write settings.json: {e}"))?;
    Ok(format!("{changed} → {}", collapse(&file)))
}

// ─────────────────────────────── Codex ───────────────────────────────

/// Write a SessionStart entry into `~/.codex/hooks.json` and ensure
/// `[features].hooks = true` in `~/.codex/config.toml`.
fn install_codex(bin: &str) -> Result<String, String> {
    let dir = home().map_err(|e| e.message.clone())?.join(".codex");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir .codex: {e}"))?;
    let file = dir.join("hooks.json");

    let mut root: serde_json::Value = if file.exists() {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("read hooks.json: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("hooks.json is not valid JSON: {e}"))?
    } else {
        serde_json::json!({})
    };
    let sessions = root
        .as_object_mut()
        .ok_or("hooks.json root is not an object")?
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]));
    let arr = sessions.as_array_mut().ok_or("SessionStart is not an array")?;
    let is_papr = |cmd: &str| cmd == "papr" || cmd.ends_with("/papr");
    let mut found = false;
    for h in arr.iter_mut() {
        if let Some(cmd) = h.get("command").and_then(|c| c.as_str()) {
            if is_papr(cmd) {
                found = true;
                if cmd != bin {
                    h["command"] = serde_json::json!(bin);
                }
            }
        }
    }
    if !found {
        arr.push(serde_json::json!({ "type": "command", "command": bin }));
    }
    let pretty =
        serde_json::to_string_pretty(&root).map_err(|e| format!("serialize hooks.json: {e}"))?;
    std::fs::write(&file, pretty + "\n").map_err(|e| format!("write hooks.json: {e}"))?;

    // Enable the hooks feature in config.toml (text-level, to avoid a TOML dep).
    let cfg = dir.join("config.toml");
    let existing = std::fs::read_to_string(&cfg).unwrap_or_default();
    if !existing.contains("hooks = true") {
        let mut next = existing.clone();
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str("\n[features]\nhooks = true\n");
        std::fs::write(&cfg, next).map_err(|e| format!("write config.toml: {e}"))?;
    }
    Ok(format!(
        "{} → {}",
        if found { "updated" } else { "installed" },
        collapse(&file)
    ))
}

// ────────────────────────────── OpenCode ──────────────────────────────

/// Install a managed OpenCode plugin that injects the papr dashboard as ambient
/// system context at session start.
fn install_opencode(bin: &str) -> Result<String, String> {
    let dir = home()
        .map_err(|e| e.message.clone())?
        .join(".config/opencode/plugin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir opencode/plugin: {e}"))?;
    let file = dir.join("papr.js");
    let plugin = format!(
        r#"// Managed by `papr setup` — injects the Papr unread dashboard at session
// start so the agent can act on your feeds immediately. Safe to delete.
import {{ execFile }} from "node:child_process"
import {{ promisify }} from "node:util"
const run = promisify(execFile)

export const papr = async () => ({{
  "experimental.systemPrompt": async ({{ parts }}) => {{
    try {{
      const {{ stdout }} = await run({bin:?}, [], {{ timeout: 5000 }})
      if (stdout.trim()) parts.push(stdout.trim())
    }} catch (_) {{ /* papr unavailable — skip silently */ }}
  }},
}})
"#,
        bin = bin
    );
    std::fs::write(&file, plugin).map_err(|e| format!("write papr.js: {e}"))?;
    Ok(format!("installed → {}", collapse(&file)))
}

fn collapse(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if let Ok(h) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{rest}");
        }
    }
    s
}
