//! Named project workspaces group services without changing the machine budget.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

use crate::{config::Project, control::Controller};

impl Controller {
    pub fn create_project(&mut self, project: Project) -> Result<Value> {
        project.validate()?;
        let _guard = self.lock()?;
        self.reload()?;
        ensure!(
            !self
                .manifest
                .projects
                .iter()
                .any(|existing| existing.id == project.id),
            "A project workspace with id {} already exists",
            project.id
        );
        let mut candidate = self.manifest.clone();
        candidate.projects.push(project.clone());
        candidate.save(&self.config_path)?;
        self.manifest = candidate;
        Ok(
            json!({"created":true,"project":project,"message":"Project workspace created. Its private container network is created when the first service is deployed."}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Manifest;
    use std::fs;

    fn project(id: &str) -> Project {
        Project {
            id: id.into(),
            name: "Build tools".into(),
            description: String::new(),
        }
    }

    #[test]
    fn projects_are_atomic_runtime_free_and_validate_references() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("services.yaml");
        Manifest::new(vec!["not-a-real-container-engine".into()])
            .save(&config)
            .unwrap();
        let mut controller = Controller::new(dir.path().into(), config.clone()).unwrap();
        assert_eq!(
            controller.create_project(project("build-tools")).unwrap()["created"],
            true
        );
        let before = fs::read(&config).unwrap();
        assert!(controller.create_project(project("build-tools")).is_err());
        assert_eq!(fs::read(&config).unwrap(), before);
        fs::write(config.with_extension("tmp"), "preserve this file").unwrap();
        assert!(controller.create_project(project("another")).is_err());
        assert_eq!(fs::read(&config).unwrap(), before);
        assert_eq!(controller.manifest.projects.len(), 1);

        let mut manifest = Manifest::new(vec!["docker".into()]);
        let service = json!({"id":"api","runtime":"docker","observe_only":false,"image":"example/api:1","limits":{"cpus":0.5,"memory_mb":128},"project":"missing"});
        manifest
            .services
            .push(serde_json::from_value(service).unwrap());
        assert!(manifest.validate().is_err());
        manifest.projects.push(project("missing"));
        manifest.validate().unwrap();
        manifest.services[0].project = None;
        assert!(
            serde_json::to_value(&manifest.services[0])
                .unwrap()
                .get("project")
                .is_none()
        );
    }

    #[test]
    fn invalid_project_fields_and_concurrent_changes_are_rejected() {
        assert!(project("../escape").validate().is_err());
        let mut value = project("valid");
        value.name.clear();
        assert!(value.validate().is_err());
        assert!(
            serde_json::from_value::<Project>(json!({"id":"valid","name":"Name","command":["sh"]}))
                .is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("services.json");
        Manifest::new(vec!["docker".into()]).save(&config).unwrap();
        let first = Controller::new(dir.path().into(), config.clone()).unwrap();
        let _guard = first.lock().unwrap();
        let mut second = Controller::new(dir.path().into(), config).unwrap();
        assert!(second.create_project(project("tools")).is_err());
    }
}
