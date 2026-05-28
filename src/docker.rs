use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use bollard::{
    Docker,
    container::LogOutput,
    errors::Error as BollardError,
    models::{
        ContainerStatsResponse, ContainerSummary, ContainerUpdateBody, EventMessage, EventMessageTypeEnum,
        ImageSummary, Network, RestartPolicy,
        RestartPolicyNameEnum, SystemVersion, Volume,
    },
    query_parameters::{
        EventsOptions, InspectContainerOptions, ListContainersOptions, ListImagesOptions,
        ListNetworksOptions, ListVolumesOptions, LogsOptions, PruneImagesOptions,
        PruneNetworksOptions, PruneVolumesOptions, StatsOptions,
        StartContainerOptions, StopContainerOptions, RestartContainerOptions, RemoveContainerOptions,
        RemoveImageOptions, RemoveVolumeOptions,
    },
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerInfo {
    pub server_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerItem {
    pub id: String,
    pub names: Vec<String>,
    pub image: String,
    pub state: Option<String>,
    pub status: Option<String>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    pub compose_container_number: Option<String>,
    pub compose_oneoff: bool,
    pub compose_working_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageItem {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub created: i64,
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeItem {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkItem {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub created: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeProject {
    pub name: String,
    pub services: Vec<String>,
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContainerStatsSample {
    pub container_id: String,
    pub cpu_percent: f64,
    pub memory_usage: u64,
    pub memory_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub container_id: String,
    pub stream: LogStream,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
    Console,
    Stdin,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DockerUpdate {
    Connected(DockerInfo),
    Disconnected(String),
    Containers(Vec<ContainerItem>),
    Images(Vec<ImageItem>),
    Volumes(Vec<VolumeItem>),
    Networks(Vec<NetworkItem>),
    Event(EventMessage),
}

#[derive(Debug, Clone)]
pub struct DockerRuntimeConfig {
    pub request_timeout: Duration,
    pub reconnect_initial_delay: Duration,
    pub reconnect_max_delay: Duration,
    pub event_buffer: usize,
    pub stream_buffer: usize,
    pub max_streams: usize,
}

impl Default for DockerRuntimeConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(5),
            reconnect_initial_delay: Duration::from_millis(500),
            reconnect_max_delay: Duration::from_secs(15),
            event_buffer: 128,
            stream_buffer: 256,
            max_streams: 16,
        }
    }
}

#[derive(Debug, Error)]
pub enum DockerError {
    #[error("failed to connect to Docker daemon: {0}")]
    Connect(#[source] BollardError),
    #[error("Docker request timed out after {0:?}")]
    Timeout(Duration),
    #[error("Docker daemon request failed: {0}")]
    Daemon(#[source] BollardError),
    #[error("Docker stream ended unexpectedly")]
    StreamEnded,
    #[error("Docker update receiver is closed")]
    UpdateReceiverClosed,
    #[error("Docker stream capacity exhausted")]
    StreamCapacityExhausted,
    #[error("Docker is not connected")]
    NotConnected,
}

pub type DockerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, DockerError>> + Send + 'a>>;

pub trait DockerClient: Send + Sync {
    fn ping<'a>(&'a self) -> DockerFuture<'a, DockerInfo>;

    fn list_containers<'a>(&'a self) -> DockerFuture<'a, Vec<ContainerItem>>;

    fn list_images<'a>(&'a self) -> DockerFuture<'a, Vec<ImageItem>>;

    fn list_volumes<'a>(&'a self) -> DockerFuture<'a, Vec<VolumeItem>>;

    fn list_networks<'a>(&'a self) -> DockerFuture<'a, Vec<NetworkItem>>;

    fn compose_projects<'a>(&'a self) -> DockerFuture<'a, Vec<ComposeProject>>;

    fn start_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()>;

    fn stop_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()>;

    fn restart_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()>;

    fn pause_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()>;

    fn unpause_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()>;

    fn remove_container<'a>(
        &'a self,
        id: String,
        force: bool,
        remove_volumes: bool,
    ) -> DockerFuture<'a, ()>;

    fn prune_images<'a>(&'a self) -> DockerFuture<'a, ()>;

    fn prune_volumes<'a>(&'a self) -> DockerFuture<'a, ()>;

    fn prune_networks<'a>(&'a self) -> DockerFuture<'a, ()>;

    fn delete_image<'a>(&'a self, id: String, force: bool) -> DockerFuture<'a, ()>;

    fn delete_volume<'a>(&'a self, name: String) -> DockerFuture<'a, ()>;

    fn delete_network<'a>(&'a self, id: String) -> DockerFuture<'a, ()>;

    fn container_env<'a>(&'a self, id: String) -> DockerFuture<'a, Vec<(String, String)>>;

    fn container_ports<'a>(&'a self, id: String) -> DockerFuture<'a, Vec<(String, u16)>>;

    fn update_container_restart_policy<'a>(
        &'a self,
        id: String,
        policy: String,
    ) -> DockerFuture<'a, ()>;
}

#[derive(Debug, Clone)]
pub struct BollardDockerClient {
    docker: Docker,
    request_timeout: Duration,
}

impl BollardDockerClient {
    pub fn connect_with_local_defaults(request_timeout: Duration) -> Result<Self, DockerError> {
        let docker = Docker::connect_with_local_defaults().map_err(DockerError::Connect)?;
        Ok(Self {
            docker,
            request_timeout,
        })
    }

    pub fn from_docker(docker: Docker, request_timeout: Duration) -> Self {
        Self {
            docker,
            request_timeout,
        }
    }

    pub fn docker(&self) -> &Docker {
        &self.docker
    }

    async fn with_timeout<T>(
        &self,
        future: impl Future<Output = Result<T, BollardError>>,
    ) -> Result<T, DockerError> {
        timeout(self.request_timeout, future)
            .await
            .map_err(|_| DockerError::Timeout(self.request_timeout))?
            .map_err(DockerError::Daemon)
    }

    async fn ping_inner(&self) -> Result<DockerInfo, DockerError> {
        let version: SystemVersion = self.with_timeout(self.docker.version()).await?;
        Ok(DockerInfo {
            server_version: version.version,
        })
    }

    async fn list_containers_inner(&self) -> Result<Vec<ContainerItem>, DockerError> {
        let options = ListContainersOptions {
            all: true,
            ..Default::default()
        };
        let containers = self
            .with_timeout(self.docker.list_containers(Some(options)))
            .await?;

        let mut items: Vec<_> = containers.iter().map(container_to_item).collect();
        items.sort_by(|a, b| {
            let a_name = a.names.first().cloned().unwrap_or_default();
            let b_name = b.names.first().cloned().unwrap_or_default();
            a_name.cmp(&b_name)
        });
        Ok(items)
    }

    async fn list_images_inner(&self) -> Result<Vec<ImageItem>, DockerError> {
        let options = ListImagesOptions {
            all: true,
            ..Default::default()
        };
        let images = self
            .with_timeout(self.docker.list_images(Some(options)))
            .await?;

        Ok(images.iter().map(image_summary_to_item).collect())
    }

    async fn list_volumes_inner(&self) -> Result<Vec<VolumeItem>, DockerError> {
        let options = ListVolumesOptions {
            ..Default::default()
        };
        let response = self
            .with_timeout(self.docker.list_volumes(Some(options)))
            .await?;

        Ok(response
            .volumes
            .unwrap_or_default()
            .iter()
            .map(volume_to_item)
            .collect())
    }

    async fn list_networks_inner(&self) -> Result<Vec<NetworkItem>, DockerError> {
        let options = ListNetworksOptions {
            ..Default::default()
        };
        let networks = self
            .with_timeout(self.docker.list_networks(Some(options)))
            .await?;

        Ok(networks.iter().map(network_to_item).collect())
    }

    async fn compose_projects_inner(&self) -> Result<Vec<ComposeProject>, DockerError> {
        let containers = self.list_containers_inner().await?;
        Ok(compose_projects_from_containers(&containers))
    }

    async fn start_container_inner(&self, id: String) -> Result<(), DockerError> {
        self.with_timeout(
            self.docker
                .start_container(&id, None::<StartContainerOptions>),
        )
        .await?;
        Ok(())
    }

    async fn stop_container_inner(&self, id: String) -> Result<(), DockerError> {
        self.with_timeout(
            self.docker
                .stop_container(&id, None::<StopContainerOptions>),
        )
        .await?;
        Ok(())
    }

    async fn restart_container_inner(&self, id: String) -> Result<(), DockerError> {
        self.with_timeout(
            self.docker
                .restart_container(&id, None::<RestartContainerOptions>),
        )
        .await?;
        Ok(())
    }

    async fn pause_container_inner(&self, id: String) -> Result<(), DockerError> {
        self.with_timeout(self.docker.pause_container(&id))
            .await?;
        Ok(())
    }

    async fn unpause_container_inner(&self, id: String) -> Result<(), DockerError> {
        self.with_timeout(self.docker.unpause_container(&id))
            .await?;
        Ok(())
    }

    async fn remove_container_inner(
        &self,
        id: String,
        force: bool,
        remove_volumes: bool,
    ) -> Result<(), DockerError> {
        let options = RemoveContainerOptions {
            force,
            v: remove_volumes,
            ..Default::default()
        };
        self.with_timeout(self.docker.remove_container(&id, Some(options)))
            .await?;
        Ok(())
    }

    async fn prune_images_inner(&self) -> Result<(), DockerError> {
        let options = PruneImagesOptions {
            ..Default::default()
        };
        self.with_timeout(self.docker.prune_images(Some(options)))
            .await?;
        Ok(())
    }

    async fn prune_volumes_inner(&self) -> Result<(), DockerError> {
        let options = PruneVolumesOptions {
            ..Default::default()
        };
        self.with_timeout(self.docker.prune_volumes(Some(options)))
            .await?;
        Ok(())
    }

    async fn prune_networks_inner(&self) -> Result<(), DockerError> {
        let options = PruneNetworksOptions {
            ..Default::default()
        };
        self.with_timeout(self.docker.prune_networks(Some(options)))
            .await?;
        Ok(())
    }

    async fn delete_image_inner(&self, id: String, force: bool) -> Result<(), DockerError> {
        let options = if force {
            Some(RemoveImageOptions { force: true, ..Default::default() })
        } else {
            None
        };
        self.with_timeout(self.docker.remove_image(&id, options, None))
            .await?;
        Ok(())
    }

    async fn delete_volume_inner(&self, name: String) -> Result<(), DockerError> {
        self.with_timeout(self.docker.remove_volume(&name, None::<RemoveVolumeOptions>))
            .await?;
        Ok(())
    }

    async fn delete_network_inner(&self, id: String) -> Result<(), DockerError> {
        self.with_timeout(self.docker.remove_network(&id))
            .await?;
        Ok(())
    }

    async fn container_env_inner(&self, id: String) -> Result<Vec<(String, String)>, DockerError> {
        let inspect = self
            .with_timeout(
                self.docker
                    .inspect_container(&id, None::<InspectContainerOptions>),
            )
            .await?;

        let env = inspect.config.and_then(|c| c.env).unwrap_or_default();
        Ok(env
            .into_iter()
            .filter_map(|s| {
                let mut parts = s.splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next().unwrap_or_default().to_string();
                Some((key, value))
            })
            .collect())
    }

    async fn container_ports_inner(&self, id: String) -> Result<Vec<(String, u16)>, DockerError> {
        let inspect = self
            .with_timeout(
                self.docker
                    .inspect_container(&id, None::<InspectContainerOptions>),
            )
            .await?;

        let ports = inspect
            .network_settings
            .and_then(|ns| ns.ports)
            .unwrap_or_default();
        let mut result = Vec::new();
        for (container_port, bindings) in ports {
            // container_port is like "80/tcp"
            if let Some(bindings) = bindings {
                for binding in bindings {
                    if let Some(host_port) = binding.host_port
                        && let Ok(port) = host_port.parse::<u16>()
                    {
                        result.push((container_port.clone(), port));
                    }
                }
            }
        }
        Ok(result)
    }

    async fn update_container_restart_policy_inner(
        &self,
        id: String,
        policy: String,
    ) -> Result<(), DockerError> {
        let name_enum = match policy.as_str() {
            "no" => RestartPolicyNameEnum::NO,
            "always" => RestartPolicyNameEnum::ALWAYS,
            "unless-stopped" => RestartPolicyNameEnum::UNLESS_STOPPED,
            "on-failure" => RestartPolicyNameEnum::ON_FAILURE,
            _ => RestartPolicyNameEnum::NO,
        };
        let options = ContainerUpdateBody {
            restart_policy: Some(RestartPolicy {
                name: Some(name_enum),
                maximum_retry_count: None,
            }),
            ..Default::default()
        };
        self.with_timeout(self.docker.update_container(&id, options))
            .await?;
        Ok(())
    }
}

impl DockerClient for BollardDockerClient {
    fn ping<'a>(&'a self) -> DockerFuture<'a, DockerInfo> {
        Box::pin(self.ping_inner())
    }

    fn list_containers<'a>(&'a self) -> DockerFuture<'a, Vec<ContainerItem>> {
        Box::pin(self.list_containers_inner())
    }

    fn list_images<'a>(&'a self) -> DockerFuture<'a, Vec<ImageItem>> {
        Box::pin(self.list_images_inner())
    }

    fn list_volumes<'a>(&'a self) -> DockerFuture<'a, Vec<VolumeItem>> {
        Box::pin(self.list_volumes_inner())
    }

    fn list_networks<'a>(&'a self) -> DockerFuture<'a, Vec<NetworkItem>> {
        Box::pin(self.list_networks_inner())
    }

    fn compose_projects<'a>(&'a self) -> DockerFuture<'a, Vec<ComposeProject>> {
        Box::pin(self.compose_projects_inner())
    }

    fn start_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()> {
        Box::pin(self.start_container_inner(id))
    }

    fn stop_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()> {
        Box::pin(self.stop_container_inner(id))
    }

    fn restart_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()> {
        Box::pin(self.restart_container_inner(id))
    }

    fn pause_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()> {
        Box::pin(self.pause_container_inner(id))
    }

    fn unpause_container<'a>(&'a self, id: String) -> DockerFuture<'a, ()> {
        Box::pin(self.unpause_container_inner(id))
    }

    fn remove_container<'a>(
        &'a self,
        id: String,
        force: bool,
        remove_volumes: bool,
    ) -> DockerFuture<'a, ()> {
        Box::pin(self.remove_container_inner(id, force, remove_volumes))
    }

    fn prune_images<'a>(&'a self) -> DockerFuture<'a, ()> {
        Box::pin(self.prune_images_inner())
    }

    fn prune_volumes<'a>(&'a self) -> DockerFuture<'a, ()> {
        Box::pin(self.prune_volumes_inner())
    }

    fn prune_networks<'a>(&'a self) -> DockerFuture<'a, ()> {
        Box::pin(self.prune_networks_inner())
    }

    fn delete_image<'a>(&'a self, id: String, force: bool) -> DockerFuture<'a, ()> {
        Box::pin(self.delete_image_inner(id, force))
    }

    fn delete_volume<'a>(&'a self, name: String) -> DockerFuture<'a, ()> {
        Box::pin(self.delete_volume_inner(name))
    }

    fn delete_network<'a>(&'a self, id: String) -> DockerFuture<'a, ()> {
        Box::pin(self.delete_network_inner(id))
    }

    fn container_env<'a>(&'a self, id: String) -> DockerFuture<'a, Vec<(String, String)>> {
        Box::pin(self.container_env_inner(id))
    }

    fn container_ports<'a>(&'a self, id: String) -> DockerFuture<'a, Vec<(String, u16)>> {
        Box::pin(self.container_ports_inner(id))
    }

    fn update_container_restart_policy<'a>(
        &'a self,
        id: String,
        policy: String,
    ) -> DockerFuture<'a, ()> {
        Box::pin(self.update_container_restart_policy_inner(id, policy))
    }
}

#[derive(Debug)]
pub struct DockerSupervisor {
    client: Option<BollardDockerClient>,
    config: DockerRuntimeConfig,
    updates_tx: mpsc::Sender<DockerUpdate>,
    updates_rx: Option<mpsc::Receiver<DockerUpdate>>,
    shutdown: CancellationToken,
    stream_slots: Arc<Semaphore>,
    handle: Option<JoinHandle<()>>,
}

impl DockerSupervisor {
    pub fn connect(config: DockerRuntimeConfig) -> Result<Self, DockerError> {
        let client = BollardDockerClient::connect_with_local_defaults(config.request_timeout)?;
        Ok(Self::new(Some(client), config))
    }

    pub fn new(client: Option<BollardDockerClient>, config: DockerRuntimeConfig) -> Self {
        let (updates_tx, updates_rx) = mpsc::channel(config.event_buffer);
        Self {
            client,
            config: config.clone(),
            updates_tx,
            updates_rx: Some(updates_rx),
            shutdown: CancellationToken::new(),
            stream_slots: Arc::new(Semaphore::new(config.max_streams)),
            handle: None,
        }
    }

    pub fn client(&self) -> Option<BollardDockerClient> {
        self.client.clone()
    }

    pub fn start(&mut self) -> Result<mpsc::Receiver<DockerUpdate>, DockerError> {
        let updates_rx = self
            .updates_rx
            .take()
            .ok_or(DockerError::UpdateReceiverClosed)?;
        let client = self.client.clone();
        let tx = self.updates_tx.clone();
        let shutdown = self.shutdown.clone();
        let config = self.config.clone();
        self.handle = Some(tokio::spawn(async move {
            event_supervisor_loop(client, tx, shutdown, config).await;
        }));

        Ok(updates_rx)
    }

    pub async fn stop(&mut self) {
        self.shutdown.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }

    pub async fn stream_logs(
        &self,
        container_id: impl Into<String>,
    ) -> Result<mpsc::Receiver<LogChunk>, DockerError> {
        let client = self.client.clone().ok_or(DockerError::NotConnected)?;
        let permit = self
            .stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| DockerError::StreamCapacityExhausted)?;
        let container_id = container_id.into();
        let (tx, rx) = mpsc::channel(self.config.stream_buffer);
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let _permit = permit;
            stream_logs_task(client, container_id, tx, shutdown).await;
        });
        Ok(rx)
    }

    pub async fn stream_stats(
        &self,
        container_id: impl Into<String>,
    ) -> Result<mpsc::Receiver<ContainerStatsSample>, DockerError> {
        let client = self.client.clone().ok_or(DockerError::NotConnected)?;
        let permit = self
            .stream_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| DockerError::StreamCapacityExhausted)?;
        let container_id = container_id.into();
        let (tx, rx) = mpsc::channel(self.config.stream_buffer);
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let _permit = permit;
            stream_stats_task(client, container_id, tx, shutdown).await;
        });
        Ok(rx)
    }
}

async fn event_supervisor_loop(
    initial_client: Option<BollardDockerClient>,
    tx: mpsc::Sender<DockerUpdate>,
    shutdown: CancellationToken,
    config: DockerRuntimeConfig,
) {
    let client = match initial_client {
        Some(c) => c,
        None => {
            let mut delay = config.reconnect_initial_delay;
            loop {
                match Docker::connect_with_local_defaults() {
                    Ok(docker) => {
                        break BollardDockerClient::from_docker(docker, config.request_timeout);
                    }
                    Err(err) => {
                        send_update(&tx, DockerUpdate::Disconnected(err.to_string())).await;
                    }
                }
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = sleep(delay) => {}
                }
                delay = (delay * 2).min(config.reconnect_max_delay);
            }
        }
    };

    let mut reconnect_delay = config.reconnect_initial_delay;

    while !shutdown.is_cancelled() && !tx.is_closed() {
        match client.ping().await {
            Ok(info) => {
                send_update(&tx, DockerUpdate::Connected(info)).await;
                if let Ok(containers) = client.list_containers().await {
                    send_update(&tx, DockerUpdate::Containers(containers)).await;
                }
                if let Ok(images) = client.list_images().await {
                    send_update(&tx, DockerUpdate::Images(images)).await;
                }
                if let Ok(volumes) = client.list_volumes().await {
                    send_update(&tx, DockerUpdate::Volumes(volumes)).await;
                }
                if let Ok(networks) = client.list_networks().await {
                    send_update(&tx, DockerUpdate::Networks(networks)).await;
                }

                let result =
                    consume_event_stream(&client, &tx, &shutdown, config.request_timeout).await;
                if let Err(err) = result {
                    send_update(&tx, DockerUpdate::Disconnected(err.to_string())).await;
                    warn!(%err, "Docker event stream disconnected");
                }

                reconnect_delay = config.reconnect_initial_delay;
            }
            Err(err) => {
                send_update(&tx, DockerUpdate::Disconnected(err.to_string())).await;
                warn!(%err, ?reconnect_delay, "Docker daemon unavailable");
            }
        }

        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = sleep(reconnect_delay) => {}
        }

        reconnect_delay = (reconnect_delay * 2).min(config.reconnect_max_delay);
    }
}

async fn consume_event_stream(
    client: &BollardDockerClient,
    tx: &mpsc::Sender<DockerUpdate>,
    shutdown: &CancellationToken,
    request_timeout: Duration,
) -> Result<(), DockerError> {
    let options = EventsOptions {
        since: None,
        until: None,
        filters: Some(HashMap::<String, Vec<String>>::new()),
    };
    let mut stream = client.docker().events(Some(options));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            item = timeout(request_timeout, stream.next()) => {
                match item {
                    Ok(Some(Ok(event))) => {
                        let event_type = event.typ;
                        send_update(tx, DockerUpdate::Event(event)).await;
                        let refresh = async {
                            match event_type {
                                Some(EventMessageTypeEnum::CONTAINER) => {
                                    let containers = client.list_containers().await?;
                                    send_update(tx, DockerUpdate::Containers(containers)).await;
                                }
                                Some(EventMessageTypeEnum::IMAGE) => {
                                    let images = client.list_images().await?;
                                    send_update(tx, DockerUpdate::Images(images)).await;
                                }
                                Some(EventMessageTypeEnum::VOLUME) => {
                                    let volumes = client.list_volumes().await?;
                                    send_update(tx, DockerUpdate::Volumes(volumes)).await;
                                }
                                Some(EventMessageTypeEnum::NETWORK) => {
                                    let networks = client.list_networks().await?;
                                    send_update(tx, DockerUpdate::Networks(networks)).await;
                                }
                                _ => {
                                    // Unknown or missing event type: refresh everything
                                    let containers = client.list_containers().await?;
                                    send_update(tx, DockerUpdate::Containers(containers)).await;
                                    let images = client.list_images().await?;
                                    send_update(tx, DockerUpdate::Images(images)).await;
                                    let volumes = client.list_volumes().await?;
                                    send_update(tx, DockerUpdate::Volumes(volumes)).await;
                                    let networks = client.list_networks().await?;
                                    send_update(tx, DockerUpdate::Networks(networks)).await;
                                }
                            }
                            Ok::<(), DockerError>(())
                        };
                        tokio::select! {
                            _ = shutdown.cancelled() => return Ok(()),
                            result = refresh => {
                                result?
                            }
                        }
                    }
                    Ok(Some(Err(err))) => return Err(DockerError::Daemon(err)),
                    Ok(None) => return Err(DockerError::StreamEnded),
                    Err(_) => return Err(DockerError::Timeout(request_timeout)),
                }
            }
        }
    }
}

async fn stream_logs_task(
    client: BollardDockerClient,
    container_id: String,
    tx: mpsc::Sender<LogChunk>,
    shutdown: CancellationToken,
) {
    let options = LogsOptions {
        follow: true,
        stdout: true,
        stderr: true,
        timestamps: false,
        tail: "200".to_string(),
        ..Default::default()
    };
    let mut stream = client.docker().logs(&container_id, Some(options));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            item = stream.next() => {
                match item {
                    Some(Ok(output)) => {
                        let chunk = log_output_to_chunk(&container_id, output);
                        send_stream(&tx, chunk).await;
                    }
                    Some(Err(err)) => {
                        warn!(%err, container_id, "container log stream failed");
                        return;
                    }
                    None => return,
                }
            }
        }
    }
}

async fn stream_stats_task(
    client: BollardDockerClient,
    container_id: String,
    tx: mpsc::Sender<ContainerStatsSample>,
    shutdown: CancellationToken,
) {
    let options = StatsOptions {
        stream: true,
        one_shot: false,
    };
    let mut stream = client.docker().stats(&container_id, Some(options));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            item = stream.next() => {
                match item {
                    Some(Ok(stats)) => {
                        let sample = ContainerStatsSample {
                            container_id: container_id.clone(),
                            cpu_percent: calculate_cpu_percent(&stats),
                            memory_usage: stats.memory_stats.as_ref().and_then(|memory| memory.usage).unwrap_or_default(),
                            memory_limit: stats.memory_stats.as_ref().and_then(|memory| memory.limit).unwrap_or_default(),
                        };
                        send_stream(&tx, sample).await;
                    }
                    Some(Err(err)) => {
                        warn!(%err, container_id, "container stats stream failed");
                        return;
                    }
                    None => return,
                }
            }
        }
    }
}

async fn send_update(tx: &mpsc::Sender<DockerUpdate>, update: DockerUpdate) {
    send_critical(tx, update).await;
}

/// Send a critical update. If the channel is full, waits up to 5 seconds then drops
/// the item to prevent the supervisor from hanging during shutdown.
async fn send_critical<T>(tx: &mpsc::Sender<T>, item: T) {
    match tx.try_send(item) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(item)) => {
            let _ = timeout(Duration::from_secs(5), tx.send(item)).await;
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

/// Send a stream sample (logs/stats). Drops the item if the channel is full to prevent backpressure.
async fn send_stream<T>(tx: &mpsc::Sender<T>, item: T) {
    match tx.try_send(item) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            debug!("dropping stream sample due to backpressure");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

fn container_to_item(summary: &ContainerSummary) -> ContainerItem {
    let labels = summary.labels.as_ref();
    let label = |key: &str| labels.and_then(|items| items.get(key)).cloned();
    ContainerItem {
        id: summary.id.clone().unwrap_or_default(),
        names: summary.names.clone().unwrap_or_default(),
        image: summary.image.clone().unwrap_or_default(),
        state: summary.state.map(|state| state.to_string()),
        status: summary.status.clone(),
        compose_project: label("com.docker.compose.project"),
        compose_service: label("com.docker.compose.service"),
        compose_container_number: label("com.docker.compose.container-number")
            .or_else(|| label("com.docker.compose.container")),
        compose_oneoff: label("com.docker.compose.oneoff").as_deref() == Some("True"),
        compose_working_dir: label("com.docker.compose.project.working_dir"),
    }
}

fn image_summary_to_item(summary: &ImageSummary) -> ImageItem {
    ImageItem {
        id: summary.id.clone(),
        repo_tags: summary.repo_tags.clone(),
        created: summary.created,
        size: summary.size,
    }
}

fn volume_to_item(volume: &Volume) -> VolumeItem {
    VolumeItem {
        name: volume.name.clone(),
        driver: volume.driver.clone(),
        mountpoint: volume.mountpoint.clone(),
        created_at: volume.created_at.clone(),
    }
}

fn network_to_item(network: &Network) -> NetworkItem {
    NetworkItem {
        id: network.id.clone().unwrap_or_default(),
        name: network.name.clone().unwrap_or_default(),
        driver: network.driver.clone().unwrap_or_default(),
        created: network.created.clone(),
    }
}

pub fn compose_projects_from_containers(containers: &[ContainerItem]) -> Vec<ComposeProject> {
    let mut projects: HashMap<String, (Vec<String>, Option<String>)> = HashMap::new();

    for container in containers.iter() {
        let Some(project) = container.compose_project.clone() else {
            continue;
        };
        let Some(service) = container.compose_service.clone() else {
            continue;
        };
        if container.compose_oneoff {
            continue;
        }

        let entry = projects.entry(project).or_default();
        if !entry.0.contains(&service) {
            entry.0.push(service);
        }
        if entry.1.is_none() {
            entry.1 = container.compose_working_dir.clone();
        }
    }

    let mut projects: Vec<_> = projects
        .into_iter()
        .map(|(name, (mut services, working_dir))| {
            services.sort();
            ComposeProject { name, services, working_dir }
        })
        .collect();
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    projects
}

fn log_output_to_chunk(container_id: &str, output: LogOutput) -> LogChunk {
    match output {
        LogOutput::StdOut { message } => LogChunk {
            container_id: container_id.to_string(),
            stream: LogStream::Stdout,
            bytes: message.to_vec(),
        },
        LogOutput::StdErr { message } => LogChunk {
            container_id: container_id.to_string(),
            stream: LogStream::Stderr,
            bytes: message.to_vec(),
        },
        LogOutput::Console { message } => LogChunk {
            container_id: container_id.to_string(),
            stream: LogStream::Console,
            bytes: message.to_vec(),
        },
        LogOutput::StdIn { message } => LogChunk {
            container_id: container_id.to_string(),
            stream: LogStream::Stdin,
            bytes: message.to_vec(),
        },
    }
}

fn calculate_cpu_percent(stats: &ContainerStatsResponse) -> f64 {
    let Some(cpu_stats) = stats.cpu_stats.as_ref() else {
        return 0.0;
    };
    let Some(precpu_stats) = stats.precpu_stats.as_ref() else {
        return 0.0;
    };
    let Some(cpu_usage) = cpu_stats.cpu_usage.as_ref() else {
        return 0.0;
    };
    let Some(precpu_usage) = precpu_stats.cpu_usage.as_ref() else {
        return 0.0;
    };

    let cpu_delta = cpu_usage.total_usage.unwrap_or_default() as f64
        - precpu_usage.total_usage.unwrap_or_default() as f64;
    let system_delta = cpu_stats.system_cpu_usage.unwrap_or_default() as f64
        - precpu_stats.system_cpu_usage.unwrap_or_default() as f64;
    let online_cpus = cpu_stats.online_cpus.unwrap_or_else(|| {
        cpu_usage
            .percpu_usage
            .as_ref()
            .map_or(1, |percpu_usage| percpu_usage.len() as u32)
    }) as f64;

    if cpu_delta <= 0.0 || system_delta <= 0.0 {
        return 0.0;
    }

    (cpu_delta / system_delta) * online_cpus * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_compose_projects_from_container_labels() {
        let containers = vec![
            ContainerItem {
                id: "1".to_string(),
                names: vec!["web".to_string()],
                image: "nginx".to_string(),
                state: Some("running".to_string()),
                status: None,
                compose_project: Some("app".to_string()),
                compose_service: Some("web".to_string()),
                compose_container_number: Some("1".to_string()),
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "2".to_string(),
                names: vec!["job".to_string()],
                image: "busybox".to_string(),
                state: Some("exited".to_string()),
                status: None,
                compose_project: Some("app".to_string()),
                compose_service: Some("job".to_string()),
                compose_container_number: Some("1".to_string()),
                compose_oneoff: true,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "3".to_string(),
                names: vec!["db".to_string()],
                image: "postgres".to_string(),
                state: Some("running".to_string()),
                status: None,
                compose_project: Some("app".to_string()),
                compose_service: Some("db".to_string()),
                compose_container_number: Some("1".to_string()),
                compose_oneoff: false,
                compose_working_dir: None,
            },
        ];

        let projects = compose_projects_from_containers(&containers);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "app");
        assert_eq!(projects[0].services, vec!["db", "web"]);
    }

    #[test]
    fn sort_containers_by_first_name() {
        let mut containers = [ContainerItem {
                id: "1".to_string(),
                names: vec!["zebra".to_string()],
                image: "z".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "2".to_string(),
                names: vec!["alpha".to_string()],
                image: "a".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "3".to_string(),
                names: vec!["beta".to_string()],
                image: "b".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            }];
        containers.sort_by(|a, b| {
            let a_name = a.names.first().cloned().unwrap_or_default();
            let b_name = b.names.first().cloned().unwrap_or_default();
            a_name.cmp(&b_name)
        });

        assert_eq!(containers[0].names[0], "alpha");
        assert_eq!(containers[1].names[0], "beta");
        assert_eq!(containers[2].names[0], "zebra");
    }
}

#[cfg(test)]
mod integration_tests {
    use std::time::Instant;

    use bollard::{
        models::ContainerCreateBody,
        query_parameters::{
            CreateContainerOptions, CreateImageOptions, RemoveContainerOptionsBuilder,
            StartContainerOptions,
        },
    };

    use super::*;

    fn integration_enabled() -> bool {
        std::env::var("ERIO_DOCKER_TESTS").as_deref() == Ok("1")
    }

    async fn test_client() -> Option<BollardDockerClient> {
        if !integration_enabled() {
            return None;
        }

        let client =
            BollardDockerClient::connect_with_local_defaults(Duration::from_secs(10)).ok()?;
        match client.ping().await {
            Ok(_) => Some(client),
            Err(err) => {
                eprintln!("skipping Docker integration test: {err}");
                None
            }
        }
    }

    async fn ensure_busybox(client: &BollardDockerClient) {
        let mut stream = client.docker().create_image(
            Some(CreateImageOptions {
                from_image: Some("busybox".to_string()),
                tag: Some("latest".to_string()),
                ..Default::default()
            }),
            None,
            None,
        );
        while stream.next().await.is_some() {}
    }

    #[tokio::test]
    async fn integration_lists_containers_and_derives_compose_metadata() {
        let Some(client) = test_client().await else {
            return;
        };
        let containers = client.list_containers().await.unwrap();
        let _projects = compose_projects_from_containers(&containers);
    }

    #[tokio::test]
    async fn integration_streams_logs_and_stats_with_backpressure() {
        let Some(client) = test_client().await else {
            return;
        };
        ensure_busybox(&client).await;

        let name = format!("erio-stream-{}", std::process::id());
        let options = CreateContainerOptions {
            name: Some(name.clone()),
            ..Default::default()
        };
        let config = ContainerCreateBody {
            image: Some("busybox:latest".to_string()),
            cmd: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "i=0; while [ $i -lt 20 ]; do echo erio-$i; i=$((i+1)); sleep 0.05; done"
                    .to_string(),
            ]),
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            ..Default::default()
        };
        let created = client
            .docker()
            .create_container(Some(options), config)
            .await
            .unwrap();
        client
            .docker()
            .start_container(&created.id, None::<StartContainerOptions>)
            .await
            .unwrap();

        let config = DockerRuntimeConfig {
            stream_buffer: 2,
            max_streams: 2,
            ..Default::default()
        };
        let supervisor = DockerSupervisor::new(Some(client.clone()), config);
        let mut logs = supervisor.stream_logs(created.id.clone()).await.unwrap();
        let mut stats = supervisor.stream_stats(created.id.clone()).await.unwrap();

        let started = Instant::now();
        let mut saw_log = false;
        let mut saw_stats = false;
        while started.elapsed() < Duration::from_secs(10) && (!saw_log || !saw_stats) {
            tokio::select! {
                item = logs.recv() => saw_log |= item.is_some(),
                item = stats.recv() => saw_stats |= item.is_some(),
                _ = sleep(Duration::from_millis(50)) => {}
            }
        }

        let _ = client
            .docker()
            .remove_container(
                &created.id,
                Some(RemoveContainerOptionsBuilder::new().force(true).build()),
            )
            .await;

        assert!(saw_log, "expected at least one log chunk");
        assert!(saw_stats, "expected at least one stats sample");
    }

    #[tokio::test]
    async fn integration_supervisor_publishes_realtime_container_updates() {
        let Some(client) = test_client().await else {
            return;
        };

        let mut supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut updates = supervisor.start().unwrap();

        let started = Instant::now();
        let mut saw_container_snapshot = false;
        while started.elapsed() < Duration::from_secs(10) {
            match updates.recv().await {
                Some(DockerUpdate::Containers(_)) => {
                    saw_container_snapshot = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        supervisor.stop().await;

        assert!(saw_container_snapshot);
    }
}
