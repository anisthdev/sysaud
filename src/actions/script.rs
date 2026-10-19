use crate::event::handler::EventHandler;
use crate::event::Event;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Scripts are meant to be short reactions (e.g. a ~1.5s overlay); anything
/// still running after this is killed along with its children.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ScriptHandler {
    config: crate::config::SharedConfig,
}

impl ScriptHandler {
    pub fn new(config: crate::config::SharedConfig) -> Self {
        Self { config }
    }
}

impl EventHandler for ScriptHandler {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        let config = self.config.snapshot();
        let event_id_str = event.id.as_str();

        if let Some(ov) = config.events.get(&event_id_str) {
            if !ov.enabled {
                return Ok(());
            }
            if let Some(script) = &ov.script {
                run_script(script, event, &config.daemon.lock_app);
            }
        }
        Ok(())
    }
}

fn run_script(script: &str, event: &Event, lock_app: &str) {
    run_script_with_timeout(script, event, lock_app, SCRIPT_TIMEOUT);
}

fn run_script_with_timeout(script: &str, event: &Event, lock_app: &str, timeout: Duration) {
    let env_vars = build_env_vars(event, lock_app);

    // Own process group so a timeout can kill everything the script started.
    let spawned = Command::new("sh")
        .arg("-c")
        .arg(script)
        .envs(env_vars)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("Script '{}' failed to spawn: {}", script, e);
            return;
        }
    };

    // Run in the background so a slow script doesn't hold up the event loop.
    let script = script.to_string();
    tokio::spawn(async move {
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                tracing::debug!("Script '{}' completed", script);
            }
            Ok(Ok(status)) => {
                tracing::warn!(
                    "Script '{}' exited with {}",
                    script,
                    status.code().unwrap_or(-1)
                );
            }
            Ok(Err(e)) => {
                tracing::warn!("Script '{}' failed while waiting: {}", script, e);
            }
            Err(_) => {
                tracing::warn!("Script '{}' timed out after {:?}, killing it", script, timeout);
                if let Some(pid) = child.id() {
                    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
                }
                let _ = child.wait().await;
            }
        }
    });
}

fn build_env_vars(event: &Event, lock_app: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("SYSAUD_EVENT".to_string(), event.id.as_str());
    env.insert("SYSAUD_CATEGORY".to_string(), event.id.category.clone());
    env.insert("SYSAUD_NAME".to_string(), event.id.name.clone());

    for (key, value) in &event.detail {
        let env_key = format!("SYSAUD_DETAIL_{}", key.to_uppercase());
        env.insert(env_key, value.clone());
    }

    env.insert("SYSAUD_LOCK_APP".to_string(), lock_app.to_string());

    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sysaud-script-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn wait_for(path: &std::path::Path) -> String {
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(path) {
                if !s.is_empty() {
                    return s;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{} never appeared", path.display());
    }

    #[tokio::test]
    async fn runs_in_background_with_event_env() {
        let dir = scratch_dir("env");
        let out = dir.join("out");
        let event = Event::new("power", "ac-plugged", HashMap::new());

        let started = Instant::now();
        run_script(&format!("sleep 0.3; echo \"$SYSAUD_EVENT $SYSAUD_LOCK_APP\" > {}", out.display()), &event, "hyprlock");
        assert!(started.elapsed() < Duration::from_millis(100), "run_script blocked");

        assert_eq!(wait_for(&out).await.trim(), "power.ac-plugged hyprlock");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_process_group() {
        let dir = scratch_dir("timeout");
        let pidfile = dir.join("pid");
        let event = Event::new("power", "ac-plugged", HashMap::new());

        run_script_with_timeout(
            &format!("sleep 30 & echo $! > {}; wait", pidfile.display()),
            &event,
            "hyprlock",
            Duration::from_millis(300),
        );

        let pid: i32 = wait_for(&pidfile).await.trim().parse().unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "grandchild sleep {pid} survived the timeout");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
