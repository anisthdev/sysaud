use crate::event::handler::EventHandler;
use crate::event::Event;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

pub struct ScriptHandler {
    config: Arc<crate::config::Config>,
}

impl ScriptHandler {
    pub fn new(config: Arc<crate::config::Config>) -> Self {
        Self { config }
    }
}

impl EventHandler for ScriptHandler {
    fn handle(&self, event: &Event) -> anyhow::Result<()> {
        let event_id_str = event.id.as_str();

        if let Some(ov) = self.config.events.get(&event_id_str) {
            if !ov.enabled {
                return Ok(());
            }
            if let Some(script) = &ov.script {
                run_script(script, event, &self.config.daemon.lock_app);
            }
        }
        Ok(())
    }
}

fn run_script(script: &str, event: &Event, lock_app: &str) {
    let env_vars = build_env_vars(event, lock_app);

    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .envs(env_vars)
        .output();

    match output {
        Ok(out) => {
            if !out.status.success() {
                tracing::warn!(
                    "Script '{}' exited with {}",
                    script,
                    out.status.code().unwrap_or(-1)
                );
            } else {
                tracing::debug!("Script '{}' completed", script);
            }
        }
        Err(e) => {
            tracing::warn!("Script '{}' failed to spawn: {}", script, e);
        }
    }
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
