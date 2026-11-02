//! Settings shared by the local app and controller.
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::control::Controller;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    pub max_parallel_jobs: usize,
}

impl SettingsUpdate {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=256).contains(&self.max_parallel_jobs),
            "Concurrent jobs must be between 1 and 256"
        );
        Ok(())
    }
}

impl Controller {
    pub fn settings(&self) -> Value {
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "controller": {"max_parallel_jobs": self.manifest.budget.max_parallel_jobs},
            "history": self.history_status(),
            "paths": {
                "workspace": self.workspace,
                "config": self.config_path,
                "state": self.workspace.join("state.json"),
                "history": self.workspace.join("history.sqlite3")
            }
        })
    }

    pub fn update_settings(&mut self, update: SettingsUpdate) -> Result<Value> {
        update.validate()?;
        let _guard = self.lock()?;
        self.reload()?;
        let mut candidate = self.manifest.clone();
        candidate.budget.max_parallel_jobs = update.max_parallel_jobs;
        candidate.save(&self.config_path)?;
        self.manifest = candidate;
        Ok(json!({
            "ok": true,
            "controller": {"max_parallel_jobs": update.max_parallel_jobs},
            "message": "Settings saved. Running jobs continue; the new concurrency limit applies when admitting another job."
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Manifest;

    #[test]
    fn settings_validate_and_persist_without_changing_other_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("services.yaml");
        Manifest::new(vec!["missing-engine".into()])
            .save(&config)
            .unwrap();
        let mut controller = Controller::new(dir.path().into(), config.clone()).unwrap();
        let before = serde_json::to_value(&controller.manifest).unwrap();
        controller
            .update_settings(SettingsUpdate {
                max_parallel_jobs: 4,
            })
            .unwrap();
        let mut expected = before;
        expected["budget"]["max_parallel_jobs"] = json!(4);
        assert_eq!(
            serde_json::to_value(Manifest::load(&config).unwrap()).unwrap(),
            expected
        );
        let bytes = std::fs::read(&config).unwrap();
        for invalid in [0, 257] {
            assert!(
                controller
                    .update_settings(SettingsUpdate {
                        max_parallel_jobs: invalid
                    })
                    .is_err()
            );
            assert_eq!(std::fs::read(&config).unwrap(), bytes);
        }
        assert!(
            serde_json::from_value::<SettingsUpdate>(
                json!({"max_parallel_jobs":2,"engine":["sh"]})
            )
            .is_err()
        );
        std::fs::write(config.with_extension("tmp"), "preserve").unwrap();
        assert!(
            controller
                .update_settings(SettingsUpdate {
                    max_parallel_jobs: 2
                })
                .is_err()
        );
        assert_eq!(controller.manifest.budget.max_parallel_jobs, 4);
        assert_eq!(std::fs::read(&config).unwrap(), bytes);
    }
}
