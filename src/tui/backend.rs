use super::{Operation, Update};
use anyhow::{Context, Result, bail, ensure};
use hyper::Method;
use init_core::{
    control::{AllocationRequest, Controller, DeploymentRequest, DeploymentService},
    machines::{self, Machine, Store},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) enum Target {
    Local,
    Remote(Machine),
}

pub(super) struct Backend {
    local: Controller,
    workspace: PathBuf,
    target: Target,
    generation: u64,
}

impl Backend {
    pub(super) fn new(local: Controller) -> Self {
        Self {
            workspace: local.workspace.clone(),
            local,
            target: Target::Local,
            generation: 0,
        }
    }

    pub(super) fn machines(&self) -> Result<Vec<Machine>> {
        Ok(Store::load(&self.workspace)?.list().to_vec())
    }

    pub(super) fn set_target(&mut self, target: Target, generation: u64) -> Result<()> {
        self.target = target;
        self.generation = generation;
        if let Target::Remote(machine) = &self.target {
            validate_pin(&self.workspace, machine)?;
        }
        Ok(())
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) async fn snapshot(&mut self) -> Result<Value> {
        match &self.target {
            Target::Local => Ok(self.local.snapshot().await),
            Target::Remote(_) => self.remote_request(RequestPlan::get("/api/state")).await,
        }
    }

    async fn remote_request(&self, plan: RequestPlan) -> Result<Value> {
        let Target::Remote(machine) = &self.target else {
            bail!("No remote machine selected");
        };
        validate_pin(&self.workspace, machine)?;
        decode_reply(machines::request(machine, &plan.method, &plan.path, &plan.body, false).await?)
    }

    async fn deploy(&mut self, service: DeploymentService, apply: bool) -> Result<Value> {
        match &self.target {
            Target::Local => {
                self.local
                    .deploy(DeploymentRequest {
                        apply,
                        service,
                        github: None,
                    })
                    .await
            }
            Target::Remote(_) => self.remote_request(deployment_plan(&service, apply)?).await,
        }
    }

    async fn logs(&self, id: &str) -> Result<String> {
        match &self.target {
            Target::Local => self.local.logs(id, 200).await,
            Target::Remote(_) => {
                let reply = self
                    .remote_request(RequestPlan::get(format!(
                        "/api/services/{id}/logs?tail=200"
                    )))
                    .await?;
                reply["logs"]
                    .as_str()
                    .map(str::to_owned)
                    .context("Remote controller omitted log text")
            }
        }
    }

    async fn create_project(&mut self, project: init_core::config::Project) -> Result<()> {
        match &self.target {
            Target::Local => self.local.create_project(project).map(|_| ()),
            Target::Remote(_) => self
                .remote_request(RequestPlan::post(
                    "/api/projects",
                    serde_json::to_value(project)?,
                )?)
                .await
                .map(|_| ()),
        }
    }

    async fn mutation(&mut self, operation: Operation) -> Result<Option<Value>> {
        if matches!(&self.target, Target::Remote(_)) {
            let preview = matches!(&operation, Operation::Preview(_));
            let result = self.remote_request(remote_plan(&operation)?).await?;
            return Ok(preview.then_some(result));
        }
        match operation {
            Operation::Service(id, action) => {
                self.local.action(&id, action, None).await.map(|_| None)
            }
            Operation::Scale(id, limits) => self
                .local
                .action(&id, "apply", Some(limits))
                .await
                .map(|_| None),
            Operation::Budget(limits) => self.local.set_budget(limits).await.map(|_| None),
            Operation::Preview(enabled) => self
                .local
                .configure_allocation(AllocationRequest {
                    enabled: Some(enabled),
                    ..Default::default()
                })
                .await
                .map(Some),
            Operation::Allocate(enabled) => self
                .local
                .configure_allocation(AllocationRequest {
                    enabled: Some(enabled),
                    apply: true,
                    ..Default::default()
                })
                .await
                .map(|_| None),
            Operation::Deploy(service) => self.deploy(*service, true).await.map(|_| None),
            _ => unreachable!("Read operations are handled before mutations"),
        }
    }

    pub(super) async fn execute(&mut self, operation: Operation) -> Update {
        match operation {
            Operation::DeployPreview { path, project } => {
                let result = async {
                    let service = load_service(&path, project)?;
                    let preview = self.deploy(service.clone(), false).await?;
                    Ok((Box::new(service), preview))
                }
                .await;
                Update::Deployment(result)
            }
            Operation::Logs(service_id) => Update::Logs {
                result: self.logs(&service_id).await,
                service_id,
            },
            Operation::CreateProject(project) => Update::ProjectCreated {
                project_id: project.id.clone(),
                result: self.create_project(project).await,
            },
            Operation::Machines => Update::Machines(self.machines()),
            operation => Update::Completed(self.mutation(operation).await),
        }
    }
}

fn validate_pin(workspace: &Path, pinned: &Machine) -> Result<()> {
    ensure!(
        pinned.identity.is_some(),
        "Test this machine connection in Machines before using it"
    );
    let saved = Store::load(workspace)?
        .get(&pinned.profile.id)
        .context("Machine connection was removed; select a machine again")?;
    ensure!(
        saved.profile == pinned.profile
            && saved.revision == pinned.revision
            && saved.identity == pinned.identity,
        "Machine connection changed; select and test the machine again before using it"
    );
    Ok(())
}

fn load_service(path: &Path, project: Option<String>) -> Result<DeploymentService> {
    let mut service = crate::load_deployment_file(path)?;
    if service.project.is_none() {
        service.project = project;
    }
    Ok(service)
}

struct RequestPlan {
    method: Method,
    path: String,
    body: Vec<u8>,
}

impl RequestPlan {
    fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::GET,
            path: path.into(),
            body: Vec::new(),
        }
    }

    fn post(path: impl Into<String>, body: Value) -> Result<Self> {
        Ok(Self {
            method: Method::POST,
            path: path.into(),
            body: serde_json::to_vec(&body)?,
        })
    }
}

fn deployment_plan(service: &DeploymentService, apply: bool) -> Result<RequestPlan> {
    RequestPlan::post(
        "/api/services",
        json!({"apply": apply, "service": service, "github": null}),
    )
}

fn remote_plan(operation: &Operation) -> Result<RequestPlan> {
    match operation {
        Operation::Service(id, action) => {
            RequestPlan::post(format!("/api/services/{id}/{action}"), json!({}))
        }
        Operation::Scale(id, limits) => RequestPlan::post(
            format!("/api/services/{id}/apply"),
            json!({"limits": limits}),
        ),
        Operation::Budget(limits) => {
            RequestPlan::post("/api/budget", serde_json::to_value(limits)?)
        }
        Operation::Preview(enabled) => RequestPlan::post(
            "/api/allocation",
            json!({"enabled": enabled, "apply": false}),
        ),
        Operation::Allocate(enabled) => RequestPlan::post(
            "/api/allocation",
            json!({"enabled": enabled, "apply": true}),
        ),
        Operation::Deploy(service) => deployment_plan(service, true),
        _ => bail!("This operation is not a remote mutation"),
    }
}

fn decode_reply(reply: machines::Reply) -> Result<Value> {
    ensure!(
        (200..300).contains(&reply.status),
        "Remote controller returned HTTP {}: {}",
        reply.status,
        reply.body["error"]
            .as_str()
            .unwrap_or("Request failed; inspect the machine before retrying")
    );
    Ok(reply.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use init_core::{
        config::{Limits, Manifest, Project},
        machines::{Identity, Profile},
    };

    fn backend() -> (tempfile::TempDir, Backend) {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("services.json");
        Manifest::new(vec!["not-a-real-container-engine".into()])
            .save(&config)
            .unwrap();
        let controller = Controller::new(directory.path().into(), config).unwrap();
        (directory, Backend::new(controller))
    }

    fn project() -> Project {
        Project {
            id: "build-tools".into(),
            name: "Build tools".into(),
            description: "Services for building releases".into(),
        }
    }

    fn profile() -> Profile {
        Profile {
            id: "lab".into(),
            name: "Lab machine".into(),
            host: "lab.example.invalid".into(),
            ssh_port: None,
            init_port: 8787,
        }
    }

    fn identity() -> Identity {
        Identity {
            app: "init".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            workspace: "/srv/init".into(),
            config: "/srv/init/services.yaml".into(),
        }
    }

    async fn rejected_creation(backend: &mut Backend) {
        match backend.execute(Operation::CreateProject(project())).await {
            Update::ProjectCreated { project_id, result } => {
                assert_eq!(project_id, "build-tools");
                assert!(result.is_err());
            }
            _ => panic!("Workspace creation must report its destination result"),
        }
        assert!(
            Manifest::load(&backend.local.config_path)
                .unwrap()
                .projects
                .is_empty(),
            "A rejected remote operation must never create a local workspace"
        );
        assert!(backend.snapshot().await.is_err());
    }

    #[tokio::test]
    async fn workspace_creation_is_durable_and_keeps_its_id() {
        let (_directory, mut backend) = backend();
        match backend.execute(Operation::CreateProject(project())).await {
            Update::ProjectCreated { project_id, result } => {
                assert_eq!(project_id, "build-tools");
                result.unwrap();
            }
            _ => panic!("Workspace creation must report its ID"),
        }
        let manifest = Manifest::load(&backend.local.config_path).unwrap();
        assert_eq!(manifest.projects.len(), 1);
        assert_eq!(manifest.projects[0].id, "build-tools");
        assert_eq!(manifest.projects[0].name, "Build tools");
        assert_eq!(
            manifest.projects[0].description,
            "Services for building releases"
        );
        match backend.execute(Operation::CreateProject(project())).await {
            Update::ProjectCreated { result, .. } => assert!(result.is_err()),
            _ => panic!("Duplicate creation must report an error"),
        }
        assert_eq!(
            Manifest::load(&backend.local.config_path)
                .unwrap()
                .projects
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn untested_changed_and_removed_connections_cannot_fall_back_to_local() {
        let (directory, mut backend) = backend();
        let mut store = Store::load(directory.path()).unwrap();
        store.upsert(profile()).unwrap();
        assert!(
            backend
                .set_target(Target::Remote(store.get("lab").unwrap()), 1)
                .is_err()
        );
        assert_eq!(backend.generation(), 1);
        rejected_creation(&mut backend).await;

        store.verify(&profile(), identity()).unwrap();
        let pinned = store.get("lab").unwrap();
        backend.set_target(Target::Remote(pinned), 2).unwrap();
        let mut renamed = profile();
        renamed.name = "Changed connection".into();
        store.upsert(renamed.clone()).unwrap();
        rejected_creation(&mut backend).await;
        assert_eq!(backend.machines().unwrap()[0].profile.name, renamed.name);

        backend
            .set_target(Target::Remote(store.get("lab").unwrap()), 3)
            .unwrap();
        let mut changed_identity = identity();
        changed_identity.workspace = "/srv/another-init".into();
        store.verify(&renamed, changed_identity).unwrap();
        rejected_creation(&mut backend).await;

        backend
            .set_target(Target::Remote(store.get("lab").unwrap()), 4)
            .unwrap();
        store.remove("lab").unwrap();
        rejected_creation(&mut backend).await;
        match backend.execute(Operation::Machines).await {
            Update::Machines(result) => assert!(result.unwrap().is_empty()),
            _ => panic!("Machines must come from the original local store"),
        }

        backend.set_target(Target::Local, 5).unwrap();
        assert_eq!(backend.generation(), 5);
        match backend.execute(Operation::CreateProject(project())).await {
            Update::ProjectCreated { result, .. } => result.unwrap(),
            _ => panic!("Choosing this computer must recover from a remote failure"),
        }
    }

    #[tokio::test]
    async fn revision_and_identity_are_both_pinned_even_when_the_other_matches() {
        let (directory, mut backend) = backend();
        let mut store = Store::load(directory.path()).unwrap();
        store.upsert(profile()).unwrap();
        store.verify(&profile(), identity()).unwrap();
        let pinned = store.get("lab").unwrap();
        backend
            .set_target(Target::Remote(pinned.clone()), 1)
            .unwrap();

        let mut changed = pinned.clone();
        changed.revision = "new-connection-revision".into();
        std::fs::write(
            directory.path().join("machines.json"),
            serde_json::to_vec(&vec![changed]).unwrap(),
        )
        .unwrap();
        rejected_creation(&mut backend).await;

        let mut changed = pinned;
        changed.identity = None;
        std::fs::write(
            directory.path().join("machines.json"),
            serde_json::to_vec(&vec![changed]).unwrap(),
        )
        .unwrap();
        rejected_creation(&mut backend).await;
    }

    #[test]
    fn remote_actions_preserve_ids_limits_and_preview_intent() {
        let limits = Limits {
            cpus: 0.75,
            memory_mb: 384,
        };
        let cases = [
            (
                Operation::Service("worker".into(), "stop"),
                "/api/services/worker/stop",
                json!({}),
            ),
            (
                Operation::Scale("api".into(), limits.clone()),
                "/api/services/api/apply",
                json!({"limits":{"cpus":0.75,"memory_mb":384}}),
            ),
            (
                Operation::Budget(limits),
                "/api/budget",
                json!({"cpus":0.75,"memory_mb":384}),
            ),
            (
                Operation::Preview(true),
                "/api/allocation",
                json!({"enabled":true,"apply":false}),
            ),
            (
                Operation::Allocate(false),
                "/api/allocation",
                json!({"enabled":false,"apply":true}),
            ),
        ];
        for (operation, endpoint, expected_body) in cases {
            let plan = remote_plan(&operation).unwrap();
            assert_eq!(plan.method, Method::POST);
            assert_eq!(plan.path, endpoint);
            assert_eq!(
                serde_json::from_slice::<Value>(&plan.body).unwrap(),
                expected_body
            );
            assert!(machines::allowed_request(&plan.method, &plan.path));
        }
    }

    #[test]
    fn deployment_inherits_only_an_omitted_project_and_applies_the_reviewed_value() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("service.json");
        let declaration = json!({
            "id":"api", "image":"example/api:1", "limits":{"cpus":0.5,"memory_mb":128}
        });
        std::fs::write(&path, serde_json::to_vec(&declaration).unwrap()).unwrap();
        let reviewed = load_service(&path, Some("build-tools".into())).unwrap();
        assert_eq!(reviewed.project.as_deref(), Some("build-tools"));
        let preview = deployment_plan(&reviewed, false).unwrap();
        let preview_body: Value = serde_json::from_slice(&preview.body).unwrap();
        assert_eq!(preview_body["apply"], false);
        assert_eq!(preview_body["service"]["project"], "build-tools");

        let mut edited = declaration;
        edited["project"] = json!("explicit-project");
        edited["image"] = json!("example/api:changed");
        std::fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();
        let apply = remote_plan(&Operation::Deploy(Box::new(reviewed))).unwrap();
        let apply_body: Value = serde_json::from_slice(&apply.body).unwrap();
        assert_eq!(apply.path, "/api/services");
        assert_eq!(apply_body["apply"], true);
        assert_eq!(apply_body["service"]["project"], "build-tools");
        assert_eq!(apply_body["service"]["image"], "example/api:1");
        assert_eq!(
            load_service(&path, Some("build-tools".into()))
                .unwrap()
                .project
                .as_deref(),
            Some("explicit-project")
        );
    }

    #[test]
    fn remote_failures_are_errors_and_keep_the_controller_diagnostic() {
        let error = decode_reply(machines::Reply {
            status: 409,
            body: json!({"error":"Workspace already exists"}),
        })
        .unwrap_err();
        assert!(error.to_string().contains("409"));
        assert!(error.to_string().contains("Workspace already exists"));
        assert!(
            decode_reply(machines::Reply {
                status: 302,
                body: json!({}),
            })
            .is_err()
        );
        let result = decode_reply(machines::Reply {
            status: 200,
            body: json!({"services":[],"projects":[]}),
        })
        .unwrap();
        assert_eq!(result["services"], json!([]));
    }
}
