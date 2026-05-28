use std::{env, time::Duration};

use tokio::{io::AsyncBufReadExt, task::JoinSet, time};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crossterm::event::KeyCode;

use crate::{
    config::{AppConfig, TemplateVars},
    docker::{DockerClient, DockerRuntimeConfig, DockerSupervisor},
    errors::{AppError, AppResult},
    events::{AppEvent, DockerAction, EventBus, EventReceiver, ShutdownReason},
    state::{AppState, InputMode, MainContext},
    terminal::TerminalSession,
    ui::{
        AppUi,
        input::{InputAction, InputEvent, KeyStroke, map_key, read_terminal_input},
        panel::PanelId,
        renderer,
    },
};

fn parse_color(name: &str) -> ratatui::style::Color {
    match name.to_lowercase().as_str() {
        "black" => ratatui::style::Color::Black,
        "red" => ratatui::style::Color::Red,
        "green" => ratatui::style::Color::Green,
        "yellow" => ratatui::style::Color::Yellow,
        "blue" => ratatui::style::Color::Blue,
        "magenta" => ratatui::style::Color::Magenta,
        "cyan" => ratatui::style::Color::Cyan,
        "gray" | "grey" => ratatui::style::Color::Gray,
        "darkgray" | "darkgrey" => ratatui::style::Color::DarkGray,
        "lightred" => ratatui::style::Color::LightRed,
        "lightgreen" => ratatui::style::Color::LightGreen,
        "lightyellow" => ratatui::style::Color::LightYellow,
        "lightblue" => ratatui::style::Color::LightBlue,
        "lightmagenta" => ratatui::style::Color::LightMagenta,
        "lightcyan" => ratatui::style::Color::LightCyan,
        "white" => ratatui::style::Color::White,
        _ => ratatui::style::Color::White,
    }
}

pub struct App {
    config: AppConfig,
    supervisor: DockerSupervisor,
    state: AppState,
    ui: AppUi,
    event_tx: Option<tokio::sync::mpsc::Sender<AppEvent>>,
    active_stream: Option<ActiveStream>,
}

struct ActiveStream {
    container_id: String,
    context: MainContext,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
struct InputDriver {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl InputDriver {
    fn spawn(tx: tokio::sync::mpsc::Sender<AppEvent>) -> Self {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::task::spawn_blocking(move || read_input_loop(tx, task_cancel));

        Self {
            cancel,
            handle: Some(handle),
        }
    }

    async fn suspend(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }

    async fn shutdown(&mut self) {
        self.suspend().await;
    }
}

impl App {
    pub fn new(config: AppConfig) -> AppResult<Self> {
        let docker_config = DockerRuntimeConfig::default();
        let supervisor = match DockerSupervisor::connect(docker_config.clone()) {
            Ok(s) => s,
            Err(err) => {
                warn!(%err, "docker not available at startup; will retry in background");
                DockerSupervisor::new(None, docker_config)
            }
        };
        Ok(Self::with_supervisor(config, supervisor))
    }

    pub fn with_supervisor(config: AppConfig, supervisor: DockerSupervisor) -> Self {
        let state = AppState {
            log_buffer_capacity: config.log_buffer_lines.max(1),
            log_buffer_max_bytes: config.log_buffer_max_bytes.max(1_000),
            compact_mode_width: config.compact_mode_width,
            scroll_past_bottom: config.scroll_past_bottom,
            ignore_patterns: config.ignore.clone(),
            theme_border: parse_color(&config.theme.border_color),
            theme_selection: parse_color(&config.theme.selection_color),
            theme_status: parse_color(&config.theme.status_color),
            theme_error: parse_color(&config.theme.error_color),
            ..AppState::default()
        };
        Self {
            config,
            supervisor,
            state,
            ui: AppUi::default(),
            event_tx: None,
            active_stream: None,
        }
    }

    pub async fn run(mut self) -> AppResult<AppState> {
        let mut terminal = TerminalSession::enter(&self.config.terminal)?;

        let (bus, rx) = EventBus::new(256);
        self.event_tx = Some(bus.publisher());
        
        let mut tasks = JoinSet::new();
        let mut input_driver = InputDriver::spawn(bus.publisher());

        // Start Docker Supervisor
        let mut docker_updates = self.supervisor.start()?;

        // Bridge Docker updates to App events
        let docker_tx = bus.publisher();
        tasks.spawn(async move {
            while let Some(update) = docker_updates.recv().await {
                if docker_tx.send(AppEvent::DockerUpdate(update)).await.is_err() {
                    break;
                }
            }
        });

        self.spawn_runtime_tasks(&bus, &mut tasks);
        self.update_active_stream().await;

        let loop_result = self.event_loop(rx, &mut terminal, &mut input_driver).await;

        input_driver.shutdown().await;
        self.stop_active_stream();
        self.supervisor.stop().await;
        self.shutdown_tasks(tasks).await;
        terminal.shutdown()?;

        loop_result.map(|_| self.state)
    }

    fn spawn_runtime_tasks(&self, bus: &EventBus, tasks: &mut JoinSet<()>) {
        let tick_tx = bus.publisher();
        let tick_rate = Duration::from_millis(self.config.tick_rate_ms.max(1));
        tasks.spawn(async move {
            let mut interval = time::interval(tick_rate);
            loop {
                interval.tick().await;
                if tick_tx.send(AppEvent::Tick).await.is_err() {
                    break;
                }
            }
        });

        let shutdown_tx = bus.publisher();
        tasks.spawn(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {
                    let _ = shutdown_tx
                        .send(AppEvent::ShutdownRequested(ShutdownReason::CtrlC))
                        .await;
                }
                Err(err) => {
                    let _ = shutdown_tx
                        .send(AppEvent::ShutdownRequested(ShutdownReason::FatalError(
                            format!("failed to listen for Ctrl-C: {err}"),
                        )))
                        .await;
                }
            }
        });

        if self.config.docker.ping_on_startup
            && let Some(docker) = self.supervisor.client()
        {
            let docker_tx = bus.publisher();
            tasks.spawn(async move {
                let result = docker.ping().await.map_err(|err| err.to_string());
                let _ = docker_tx.send(AppEvent::DockerPinged(result)).await;
            });
        }
    }

    async fn event_loop(
        &mut self,
        mut rx: EventReceiver,
        terminal: &mut TerminalSession,
        input_driver: &mut InputDriver,
    ) -> AppResult<()> {
        info!("app event loop started");
        self.handle_event(AppEvent::Started).await?;
        renderer::draw(terminal.terminal_mut(), &mut self.ui, &self.state)?;

        while self.state.running {
            match rx.recv().await {
                Some(event) => {
                    self.handle_event(event).await?;
                    if let Some(cmd) = self.state.external_command.take() {
                        input_driver.suspend().await;
                        let result = self.run_external_command(terminal, cmd).await;
                        match self.event_tx.as_ref() {
                            Some(tx) => {
                                *input_driver = InputDriver::spawn(tx.clone());
                            }
                            None => {
                                warn!("event_tx is missing after external command; cannot respawn input driver");
                            }
                        }
                        if let Err(err) = result {
                            self.state
                                .set_error_message(format!("Command failed: {err}"));
                        }
                    }
                    renderer::draw(terminal.terminal_mut(), &mut self.ui, &self.state)?;
                }
                None => return Err(AppError::EventBusClosed),
            }
        }

        info!("app event loop stopped");
        Ok(())
    }

    async fn handle_event(&mut self, event: AppEvent) -> AppResult<()> {
        match event {
            AppEvent::Started => {
                info!("{} started", self.config.app_name);
            }
            AppEvent::Tick => {
                self.state.record_tick();
                debug!(tick_count = self.state.tick_count, "tick");
            }
            AppEvent::Input(input) => {
                self.handle_input(input).await;
            }
            AppEvent::ExecuteInputAction(action) => {
                self.dispatch_input_action(action).await;
            }
            AppEvent::DockerPinged(result) => {
                match &result {
                    Ok(info) => info!(
                        server_version = info.server_version.as_deref().unwrap_or("unknown"),
                        "docker connection available"
                    ),
                    Err(message) => warn!(%message, "docker connection unavailable"),
                }
                self.state.record_docker_ping(result);
                self.ui.render.request_redraw();
            }
            AppEvent::DockerUpdate(update) => {
                self.state.apply_docker_update(update);
                self.update_active_stream().await;
                self.ui.render.request_redraw();
            }
            AppEvent::ActionRequested(action) => {
                self.state.clear_error_message();

                if action == DockerAction::Quit {
                    self.state.request_shutdown(ShutdownReason::User);
                    return Ok(());
                }

                // External commands (exec, attach) are handled synchronously by the event loop,
                // so they don't need the event_tx channel.
                match action {
                    DockerAction::ExecShell(id) => {
                        self.state.external_command = Some(crate::state::ExternalCommand::ExecShell(id));
                        self.ui.render.request_redraw();
                        return Ok(());
                    }
                    DockerAction::AttachContainer(id) => {
                        self.state.external_command = Some(crate::state::ExternalCommand::AttachContainer(id));
                        self.ui.render.request_redraw();
                        return Ok(());
                    }
                    DockerAction::RunCustomCommand { name, command } => {
                        self.state.external_command = Some(crate::state::ExternalCommand::CustomCommand { name, command });
                        self.ui.render.request_redraw();
                        return Ok(());
                    }
                    _ => {}
                }

                let Some(tx) = &self.event_tx else { return Ok(()) };
                let tx = tx.clone();
                let Some(client) = self.supervisor.client() else {
                    self.state.set_error_message("Docker is not connected".to_string());
                    self.ui.render.request_redraw();
                    return Ok(());
                };

                match action {
                    DockerAction::StartContainer(id) => {
                        self.state.set_status_message(format!("Starting container {id}..."));
                        tokio::spawn(async move {
                            let result = client.start_container(id).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::StopContainer(id) => {
                        self.state.set_status_message(format!("Stopping container {id}..."));
                        tokio::spawn(async move {
                            let result = client.stop_container(id).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::RestartContainer(id) => {
                        self.state.set_status_message(format!("Restarting container {id}..."));
                        tokio::spawn(async move {
                            let result = client.restart_container(id).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::PauseContainer(id) => {
                        self.state.set_status_message(format!("Pausing container {id}..."));
                        tokio::spawn(async move {
                            let result = client.pause_container(id).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::UnpauseContainer(id) => {
                        self.state.set_status_message(format!("Unpausing container {id}..."));
                        tokio::spawn(async move {
                            let result = client.unpause_container(id).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::RemoveContainer(id) => {
                        self.state.set_status_message(format!("Removing container {id}..."));
                        tokio::spawn(async move {
                            let result = client.remove_container(id, false, false).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ForceRemoveContainer(id) => {
                        self.state.set_status_message(format!("Force removing container {id}..."));
                        tokio::spawn(async move {
                            let result = client.remove_container(id, true, false).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::RemoveContainerWithVolumes(id) => {
                        self.state.set_status_message(format!("Removing container {id} with volumes..."));
                        tokio::spawn(async move {
                            let result = client.remove_container(id, false, true).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::PruneImages => {
                        self.state.set_status_message("Pruning unused images...");
                        tokio::spawn(async move {
                            let result = client.prune_images().await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::PruneVolumes => {
                        self.state.set_status_message("Pruning unused volumes...");
                        tokio::spawn(async move {
                            let result = client.prune_volumes().await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::PruneNetworks => {
                        self.state.set_status_message("Pruning unused networks...");
                        tokio::spawn(async move {
                            let result = client.prune_networks().await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::DeleteImage(id) => {
                        self.state.set_status_message(format!("Deleting image {id}..."));
                        tokio::spawn(async move {
                            let result = client.delete_image(id, false).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ForceDeleteImage(id) => {
                        self.state.set_status_message(format!("Force deleting image {id}..."));
                        tokio::spawn(async move {
                            let result = client.delete_image(id, true).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::DeleteVolume(name) => {
                        self.state.set_status_message(format!("Deleting volume {name}..."));
                        tokio::spawn(async move {
                            let result = client.delete_volume(name).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::DeleteNetwork(id) => {
                        self.state.set_status_message(format!("Deleting network {id}..."));
                        tokio::spawn(async move {
                            let result = client.delete_network(id).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ProjectUp(project) => {
                        self.state.set_status_message(format!("Starting project {project}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["up", "-d"]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ProjectDown(project) => {
                        self.state.set_status_message(format!("Stopping project {project}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["down"]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ServiceUp { project, service } => {
                        self.state.set_status_message(format!("Starting service {service}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["up", "-d", &service]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ServiceStart { project, service } => {
                        self.state.set_status_message(format!("Starting service {service}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["start", &service]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ServiceStop { project, service } => {
                        self.state.set_status_message(format!("Stopping service {service}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["stop", &service]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ServiceRestart { project, service } => {
                        self.state.set_status_message(format!("Restarting service {service}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["restart", &service]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ServiceDown { project, service } => {
                        self.state.set_status_message(format!("Removing service {service}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["rm", "-fs", &service]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkStopContainers => {
                        self.state.set_status_message("Stopping all containers...");
                        let ids: Vec<String> = self.state.containers.iter().map(|c| c.id.clone()).collect();
                        tokio::spawn(async move {
                            let mut errors = Vec::new();
                            for id in ids {
                                if let Err(e) = client.stop_container(id).await {
                                    errors.push(e.to_string());
                                }
                            }
                            let result = if errors.is_empty() { Ok(()) } else { Err(errors.join(", ")) };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkStartContainers => {
                        self.state.set_status_message("Starting all containers...");
                        let ids: Vec<String> = self.state.containers.iter().map(|c| c.id.clone()).collect();
                        tokio::spawn(async move {
                            let mut errors = Vec::new();
                            for id in ids {
                                if let Err(e) = client.start_container(id).await {
                                    errors.push(e.to_string());
                                }
                            }
                            let result = if errors.is_empty() { Ok(()) } else { Err(errors.join(", ")) };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkRestartContainers => {
                        self.state.set_status_message("Restarting all containers...");
                        let ids: Vec<String> = self.state.containers.iter().map(|c| c.id.clone()).collect();
                        tokio::spawn(async move {
                            let mut errors = Vec::new();
                            for id in ids {
                                if let Err(e) = client.restart_container(id).await {
                                    errors.push(e.to_string());
                                }
                            }
                            let result = if errors.is_empty() { Ok(()) } else { Err(errors.join(", ")) };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkRemoveStoppedContainers => {
                        self.state.set_status_message("Removing stopped containers...");
                        let ids: Vec<String> = self.state.containers.iter()
                            .filter(|c| c.state.as_deref().is_some_and(|s| s == "exited" || s == "dead"))
                            .map(|c| c.id.clone())
                            .collect();
                        tokio::spawn(async move {
                            let mut errors = Vec::new();
                            for id in ids {
                                if let Err(e) = client.remove_container(id, false, false).await {
                                    errors.push(e.to_string());
                                }
                            }
                            let result = if errors.is_empty() { Ok(()) } else { Err(errors.join(", ")) };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkProjectUp => {
                        self.state.set_status_message("Starting all projects...");
                        let names: Vec<String> = self.state.projects.iter().map(|p| p.name.clone()).collect();
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let mut errors = Vec::new();
                            for name in names {
                                if let Err(e) = run_compose(&compose, &name, &["up", "-d"]).await {
                                    errors.push(e);
                                }
                            }
                            let result = if errors.is_empty() { Ok(()) } else { Err(errors.join(", ")) };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkProjectDown => {
                        self.state.set_status_message("Stopping all projects...");
                        let names: Vec<String> = self.state.projects.iter().map(|p| p.name.clone()).collect();
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let mut errors = Vec::new();
                            for name in names {
                                if let Err(e) = run_compose(&compose, &name, &["down"]).await {
                                    errors.push(e);
                                }
                            }
                            let result = if errors.is_empty() { Ok(()) } else { Err(errors.join(", ")) };
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkServiceUp(project) => {
                        self.state.set_status_message(format!("Starting all services in {project}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["up", "-d"]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkServiceStop(project) => {
                        self.state.set_status_message(format!("Stopping all services in {project}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["stop"]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::BulkServiceRestart(project) => {
                        self.state.set_status_message(format!("Restarting all services in {project}..."));
                        let compose = self.config.docker.compose_binary.clone();
                        tokio::spawn(async move {
                            let result = run_compose(&compose, &project, &["restart"]).await;
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::UpdateRestartPolicy { id, policy } => {
                        self.state.set_status_message(format!("Updating restart policy to {policy}..."));
                        tokio::spawn(async move {
                            let result = client.update_container_restart_policy(id, policy).await.map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ActionResult(result)).await;
                        });
                    }
                    DockerAction::ExecShell(_) | DockerAction::AttachContainer(_) | DockerAction::RunCustomCommand { .. } | DockerAction::OpenInBrowser(_) | DockerAction::Quit => {
                        // Handled before the match
                    }
                }
                self.ui.render.request_redraw();
            }
            AppEvent::ActionResult(result) => {
                match result {
                    Ok(()) => {
                        self.state.clear_error_message();
                        self.state.set_status_message("Action successful");
                    }
                    Err(err) => self.state.set_error_message(format!("Action failed: {err}")),
                }
                self.ui.render.request_redraw();
            }
            AppEvent::LogChunk(chunk) => {
                const MAX_PARTIAL_LINE_LEN: usize = 10_000;

                if let Some(stream) = &self.active_stream
                    && stream.container_id == chunk.container_id && stream.context == MainContext::Logs {
                        let text = String::from_utf8_lossy(&chunk.bytes).to_string();

                        // Accumulate partial lines across chunk boundaries
                        let mut buffer = String::new();
                        if let Some((partial, partial_stream)) = self.state.log_partial_line.take() {
                            if partial_stream == chunk.stream {
                                buffer.push_str(&partial);
                            } else {
                                // Stream changed, flush the old partial line
                                self.state.add_log_line(strip_ansi(&partial), partial_stream);
                            }
                        }
                        buffer.push_str(&text);

                        // Prevent unbounded growth if a container never emits a newline
                        if buffer.len() > MAX_PARTIAL_LINE_LEN {
                            let split_at = buffer.floor_char_boundary(MAX_PARTIAL_LINE_LEN);
                            let remainder = buffer.split_off(split_at);
                            self.state.add_log_line(strip_ansi(&buffer).replace('\t', "        "), chunk.stream);
                            buffer = remainder;
                        }

                        let ends_with_newline = buffer.ends_with('\n') || buffer.ends_with("\r\n");

                        let mut lines = buffer.lines().peekable();
                        while let Some(l) = lines.next() {
                            if !ends_with_newline && lines.peek().is_none() {
                                // Last line without trailing newline - save as partial
                                let partial = l.replace('\t', "        ");
                                if partial.len() > MAX_PARTIAL_LINE_LEN {
                                    self.state.add_log_line(strip_ansi(&partial).replace('\t', "        "), chunk.stream);
                                } else {
                                    self.state.log_partial_line = Some((partial, chunk.stream));
                                }
                                break;
                            }
                            let clean = strip_ansi(l).replace('\t', "        ");
                            self.state.add_log_line(clean, chunk.stream);
                        }

                        self.ui.render.request_redraw();
                    }
            }
            AppEvent::StatsSample(sample) => {
                if let Some(stream) = &self.active_stream
                    && stream.container_id == sample.container_id && stream.context == MainContext::Stats {
                        self.state.update_stats(sample);
                        self.ui.render.request_redraw();
                    }
            }
            AppEvent::EnvVarsLoaded(vars) => {
                self.state.env_vars = vars;
                self.state.main_scroll_offsets.insert(MainContext::Env, 0);
                self.ui.render.request_redraw();
            }
            AppEvent::EnvVarsFailed(err) => {
                self.state
                    .set_error_message(format!("Failed to load env vars: {err}"));
                self.ui.render.request_redraw();
            }
            AppEvent::ShutdownRequested(reason) => {
                info!(?reason, "shutdown requested");
                self.state.request_shutdown(reason);
            }
            AppEvent::ShutdownComplete => {
                self.state.running = false;
            }
        }

        Ok(())
    }

    async fn handle_input(&mut self, input: InputEvent) {
        match input {
            InputEvent::Mouse(mouse) => {
                self.handle_mouse_event(mouse).await;
            }
            InputEvent::Key(key) => {
                if self.state.input_mode == InputMode::Search {
                    self.handle_search_key(key);
                    return;
                }
                if self.state.input_mode == InputMode::Confirm {
                    self.handle_confirm_key(key).await;
                    return;
                }
                if self.state.input_mode == InputMode::Help {
                    self.state.exit_help_mode();
                    self.ui.render.request_redraw();
                    return;
                }
                if self.state.input_mode == InputMode::Menu {
                    self.handle_menu_key(key).await;
                    return;
                }
                if self.state.input_mode == InputMode::Leader {
                    self.state.input_mode = InputMode::Normal;
                    self.handle_leader_key(key).await;
                    self.ui.render.request_redraw();
                    return;
                }

                // Normal mode
                let action = map_key(key);
                match action {
                    InputAction::Leader => {
                        self.state.input_mode = InputMode::Leader;
                        self.ui.render.request_redraw();
                    }
                    InputAction::Quit
                    | InputAction::FocusNext
                    | InputAction::FocusPrevious
                    | InputAction::FocusPanel(_)
                    | InputAction::MoveUp
                    | InputAction::MoveDown
                    | InputAction::SetMainContext(_)
                    | InputAction::ScrollMainUp
                    | InputAction::ScrollMainDown
                    | InputAction::ScrollMainUpLarge
                    | InputAction::ScrollMainDownLarge
                    | InputAction::ScrollMainTop
                    | InputAction::ScrollMainBottom
                    | InputAction::ScrollMainLeft
                    | InputAction::ScrollMainRight
                    | InputAction::StartSearch
                    | InputAction::ShowHelp
                    | InputAction::Confirm
                    | InputAction::Cancel
                    | InputAction::Redraw
                    | InputAction::NextScreenMode
                    | InputAction::PreviousScreenMode => {
                        self.dispatch_input_action(action).await;
                    }
                    _ => {
                        self.state.set_status_message("Press space for actions".to_string());
                        self.ui.render.request_redraw();
                    }
                }
            }
            InputEvent::Resize { width, height } => {
                self.state.terminal_size = (width, height);
                self.sync_focus_order_for_layout();
                self.ui.invalidate_layout();
                self.ui.render.mark_resize();
            }
        }
    }

    async fn dispatch_input_action(&mut self, action: InputAction) {
        match action {
            InputAction::None => {}
            InputAction::Quit => {
                if self.config.confirm_on_quit {
                    self.state.enter_confirm_mode(
                        DockerAction::Quit,
                        "Are you sure you want to quit? (y/n)".to_string(),
                    );
                    self.ui.render.request_redraw();
                } else {
                    self.state.request_shutdown(ShutdownReason::User);
                }
            }
            InputAction::FocusNext => {
                if self.ui.focus.focus_next() {
                    self.state.focused_panel = self.ui.focus.focused();
                    self.sync_compact_focus(self.ui.focus.focused());
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            InputAction::FocusPrevious => {
                if self.ui.focus.focus_previous() {
                    self.state.focused_panel = self.ui.focus.focused();
                    self.sync_compact_focus(self.ui.focus.focused());
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            InputAction::FocusPanel(panel_id) => {
                if self.ui.focus.set_focus(panel_id) {
                    self.state.focused_panel = panel_id;
                    self.sync_compact_focus(panel_id);
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            InputAction::MoveUp => {
                if self.state.move_selection(self.ui.focus.focused(), -1) {
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            InputAction::MoveDown => {
                if self.state.move_selection(self.ui.focus.focused(), 1) {
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            InputAction::SetMainContext(context) => {
                self.state.set_main_context(context);
                self.update_active_stream().await;
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainUp => {
                self.state.scroll_main_up();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainDown => {
                self.state.scroll_main_down();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainUpLarge => {
                self.state.scroll_main_up_large();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainDownLarge => {
                self.state.scroll_main_down_large();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainTop => {
                self.state.scroll_main_to_top();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainBottom => {
                self.state.scroll_main_to_bottom();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainLeft => {
                self.state.scroll_main_left();
                self.ui.render.request_redraw();
            }
            InputAction::ScrollMainRight => {
                self.state.scroll_main_right();
                self.ui.render.request_redraw();
            }
            InputAction::NextScreenMode => {
                let next = match self.state.screen_mode {
                    None => Some(crate::ui::layout::LayoutMode::CompactSidebar),
                    Some(crate::ui::layout::LayoutMode::Normal) => Some(crate::ui::layout::LayoutMode::CompactSidebar),
                    Some(crate::ui::layout::LayoutMode::CompactSidebar) => Some(crate::ui::layout::LayoutMode::Tiny),
                    Some(crate::ui::layout::LayoutMode::Tiny) => None,
                };
                self.state.screen_mode = next;
                self.ui.invalidate_layout();
                self.ui.render.request_redraw();
            }
            InputAction::PreviousScreenMode => {
                let prev = match self.state.screen_mode {
                    None => Some(crate::ui::layout::LayoutMode::Tiny),
                    Some(crate::ui::layout::LayoutMode::Tiny) => Some(crate::ui::layout::LayoutMode::CompactSidebar),
                    Some(crate::ui::layout::LayoutMode::CompactSidebar) => Some(crate::ui::layout::LayoutMode::Normal),
                    Some(crate::ui::layout::LayoutMode::Normal) => None,
                };
                self.state.screen_mode = prev;
                self.ui.invalidate_layout();
                self.ui.render.request_redraw();
            }
            InputAction::OptionsMenu => {
                self.open_options_menu();
                self.ui.render.request_redraw();
            }
            InputAction::StartContainer => {
                let panel = self.ui.focus.focused();
                if let Some(tx) = &self.event_tx {
                    if panel == PanelId::Services {
                        if let (Some(project), Some(service)) = (
                            self.state.selected_project(),
                            self.state.selected_service_name()
                        ) {
                            let _ = tx.send(AppEvent::ActionRequested(
                                DockerAction::ServiceUp { project: project.name.clone(), service }
                            )).await;
                        }
                    } else if let Some(container) = self.state.selected_container() {
                        let _ = tx.send(AppEvent::ActionRequested(DockerAction::StartContainer(
                            container.id.clone(),
                        ))).await;
                    }
                }
            }
            InputAction::StopContainer => {
                let panel = self.ui.focus.focused();
                if let Some(tx) = &self.event_tx {
                    if panel == PanelId::Services {
                        if let (Some(project), Some(service)) = (
                            self.state.selected_project(),
                            self.state.selected_service_name()
                        ) {
                            let _ = tx.send(AppEvent::ActionRequested(
                                DockerAction::ServiceStop { project: project.name.clone(), service }
                            )).await;
                        }
                    } else if let Some(container) = self.state.selected_container() {
                        let _ = tx.send(AppEvent::ActionRequested(DockerAction::StopContainer(
                            container.id.clone(),
                        ))).await;
                    }
                }
            }
            InputAction::RestartContainer => {
                let panel = self.ui.focus.focused();
                if let Some(tx) = &self.event_tx {
                    if panel == PanelId::Services {
                        if let (Some(project), Some(service)) = (
                            self.state.selected_project(),
                            self.state.selected_service_name()
                        ) {
                            let _ = tx.send(AppEvent::ActionRequested(
                                DockerAction::ServiceRestart { project: project.name.clone(), service }
                            )).await;
                        }
                    } else if let Some(container) = self.state.selected_container() {
                        let _ = tx.send(AppEvent::ActionRequested(DockerAction::RestartContainer(
                            container.id.clone(),
                        ))).await;
                    }
                }
            }
            InputAction::RestartOptions => {
                if let Some(container) = self.state.selected_container() {
                    let name = container.names.first().cloned().unwrap_or_else(|| container.id.clone());
                    self.state.enter_menu_mode(
                        format!("Restart policy for {name}"),
                        vec![
                            ("No".to_string(), DockerAction::UpdateRestartPolicy {
                                id: container.id.clone(),
                                policy: "no".to_string(),
                            }),
                            ("On-failure".to_string(), DockerAction::UpdateRestartPolicy {
                                id: container.id.clone(),
                                policy: "on-failure".to_string(),
                            }),
                            ("Always".to_string(), DockerAction::UpdateRestartPolicy {
                                id: container.id.clone(),
                                policy: "always".to_string(),
                            }),
                            ("Unless-stopped".to_string(), DockerAction::UpdateRestartPolicy {
                                id: container.id.clone(),
                                policy: "unless-stopped".to_string(),
                            }),
                        ],
                    );
                    self.ui.render.request_redraw();
                }
            }
            InputAction::DeleteSelected => {
                let menu = match self.ui.focus.focused() {
                    PanelId::Containers => self.state.selected_container().map(|c| {
                        let name = c.names.first().cloned().unwrap_or_else(|| c.id.clone());
                        (
                            format!("Remove container {name}"),
                            vec![
                                ("Remove".to_string(), DockerAction::RemoveContainer(c.id.clone())),
                                ("Force remove".to_string(), DockerAction::ForceRemoveContainer(c.id.clone())),
                                ("Remove with volumes".to_string(), DockerAction::RemoveContainerWithVolumes(c.id.clone())),
                            ],
                        )
                    }),
                    PanelId::Images => self.state.selected_image().map(|img| {
                        let name = img
                            .repo_tags
                            .first()
                            .cloned()
                            .unwrap_or_else(|| img.id.clone());
                        (
                            format!("Delete image {name}"),
                            vec![
                                ("Delete".to_string(), DockerAction::DeleteImage(img.id.clone())),
                                ("Force delete".to_string(), DockerAction::ForceDeleteImage(img.id.clone())),
                            ],
                        )
                    }),
                    PanelId::Volumes => self.state.selected_volume().map(|vol| {
                        (
                            format!("Delete volume {}", vol.name),
                            vec![
                                ("Delete".to_string(), DockerAction::DeleteVolume(vol.name.clone())),
                            ],
                        )
                    }),
                    PanelId::Networks => self.state.selected_network().map(|net| {
                        (
                            format!("Delete network {}", net.name),
                            vec![
                                ("Delete".to_string(), DockerAction::DeleteNetwork(net.id.clone())),
                            ],
                        )
                    }),
                    PanelId::Services => {
                        if let (Some(project), Some(service)) = (
                            self.state.selected_project(),
                            self.state.selected_service_name()
                        ) {
                            Some((
                                format!("Remove service {service}"),
                                vec![
                                    ("Down".to_string(), DockerAction::ServiceDown {
                                        project: project.name.clone(),
                                        service: service.clone(),
                                    }),
                                ],
                            ))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((title, items)) = menu {
                    self.state.enter_menu_mode(title, items);
                    self.ui.render.request_redraw();
                }
            }
            InputAction::Prune => {
                let action = match self.ui.focus.focused() {
                    PanelId::Images => Some((
                        DockerAction::PruneImages,
                        "Prune all unused images".to_string(),
                    )),
                    PanelId::Volumes => Some((
                        DockerAction::PruneVolumes,
                        "Prune all unused volumes".to_string(),
                    )),
                    PanelId::Networks => Some((
                        DockerAction::PruneNetworks,
                        "Prune all unused networks".to_string(),
                    )),
                    _ => None,
                };

                if let Some((action, prompt)) = action {
                    self.state.enter_confirm_mode(action, prompt);
                    self.ui.render.request_redraw();
                }
            }
            InputAction::StartSearch => {
                let panel = self.ui.focus.focused();
                self.state.enter_search_mode(panel);
                self.ui.render.request_redraw();
            }
            InputAction::ShowHelp => {
                self.state.enter_help_mode();
                self.ui.render.request_redraw();
            }
            InputAction::ExecShell => {
                let panel = self.ui.focus.focused();
                let container = if panel == PanelId::Services {
                    if let (Some(project), Some(service)) = (
                        self.state.selected_project(),
                        self.state.selected_service_name()
                    ) {
                        self.state.container_for_service(&project.name, &service).map(|c| c.id.clone())
                    } else {
                        None
                    }
                } else {
                    self.state.selected_container().map(|c| c.id.clone())
                };
                if let Some(id) = container
                    && let Some(tx) = &self.event_tx
                {
                    let _ = tx.send(AppEvent::ActionRequested(DockerAction::ExecShell(id))).await;
                }
            }
            InputAction::AttachContainer => {
                let panel = self.ui.focus.focused();
                let container = if panel == PanelId::Services {
                    if let (Some(project), Some(service)) = (
                        self.state.selected_project(),
                        self.state.selected_service_name()
                    ) {
                        self.state.container_for_service(&project.name, &service).map(|c| c.id.clone())
                    } else {
                        None
                    }
                } else {
                    self.state.selected_container().map(|c| c.id.clone())
                };
                if let Some(id) = container
                    && let Some(tx) = &self.event_tx
                {
                    let _ = tx.send(AppEvent::ActionRequested(DockerAction::AttachContainer(id))).await;
                }
            }
            InputAction::GlobalCustomCommands => {
                if self.config.custom_commands.global.is_empty() {
                    self.state.set_status_message("No global custom commands configured".to_string());
                    self.ui.render.request_redraw();
                } else {
                    let items: Vec<(String, DockerAction)> = self.config.custom_commands.global.iter()
                        .map(|cmd| (cmd.name.clone(), DockerAction::RunCustomCommand {
                            name: cmd.name.clone(),
                            command: cmd.command.clone(),
                        }))
                        .collect();
                    self.state.enter_menu_mode("Global commands".to_string(), items);
                    self.ui.render.request_redraw();
                }
            }
            InputAction::ProjectUp => {
                if let Some(project) = self.state.selected_project()
                    && let Some(tx) = &self.event_tx
                {
                    let _ = tx
                        .send(AppEvent::ActionRequested(DockerAction::ProjectUp(
                            project.name.clone(),
                        )))
                        .await;
                }
            }
            InputAction::ProjectDown => {
                if let Some(project) = self.state.selected_project()
                    && let Some(tx) = &self.event_tx
                {
                    let _ = tx
                        .send(AppEvent::ActionRequested(DockerAction::ProjectDown(
                            project.name.clone(),
                        )))
                        .await;
                }
            }
            InputAction::BulkCommands => {
                let mut menu = match self.ui.focus.focused() {
                    PanelId::Containers => Some((
                        "Bulk container operations".to_string(),
                        vec![
                            ("Stop all".to_string(), DockerAction::BulkStopContainers),
                            ("Start all".to_string(), DockerAction::BulkStartContainers),
                            ("Restart all".to_string(), DockerAction::BulkRestartContainers),
                            ("Remove stopped".to_string(), DockerAction::BulkRemoveStoppedContainers),
                        ],
                    )),
                    PanelId::Images => Some((
                        "Bulk image operations".to_string(),
                        vec![
                            ("Prune dangling".to_string(), DockerAction::PruneImages),
                            ("Prune all unused".to_string(), DockerAction::PruneImages),
                        ],
                    )),
                    PanelId::Volumes => Some((
                        "Bulk volume operations".to_string(),
                        vec![
                            ("Prune all unused".to_string(), DockerAction::PruneVolumes),
                        ],
                    )),
                    PanelId::Networks => Some((
                        "Bulk network operations".to_string(),
                        vec![
                            ("Prune all unused".to_string(), DockerAction::PruneNetworks),
                        ],
                    )),
                    PanelId::Projects => Some((
                        "Bulk project operations".to_string(),
                        vec![
                            ("Up all".to_string(), DockerAction::BulkProjectUp),
                            ("Down all".to_string(), DockerAction::BulkProjectDown),
                        ],
                    )),
                    PanelId::Services => {
                        self.state.selected_project().map(|p| (
                            "Bulk service operations".to_string(),
                            vec![
                                ("Up all".to_string(), DockerAction::BulkServiceUp(p.name.clone())),
                                ("Stop all".to_string(), DockerAction::BulkServiceStop(p.name.clone())),
                                ("Restart all".to_string(), DockerAction::BulkServiceRestart(p.name.clone())),
                            ],
                        ))
                    }
                    PanelId::Main => None,
                    PanelId::Status => None,
                };

                if let Some((ref _title, ref mut items)) = menu {
                    let custom: Vec<(String, DockerAction)> = match self.ui.focus.focused() {
                        PanelId::Containers => self.config.custom_commands.bulk_containers.iter().map(|c| (c.name.clone(), DockerAction::RunCustomCommand { name: c.name.clone(), command: c.command.clone() })).collect(),
                        PanelId::Images => self.config.custom_commands.bulk_images.iter().map(|c| (c.name.clone(), DockerAction::RunCustomCommand { name: c.name.clone(), command: c.command.clone() })).collect(),
                        PanelId::Volumes => self.config.custom_commands.bulk_volumes.iter().map(|c| (c.name.clone(), DockerAction::RunCustomCommand { name: c.name.clone(), command: c.command.clone() })).collect(),
                        PanelId::Networks => self.config.custom_commands.bulk_networks.iter().map(|c| (c.name.clone(), DockerAction::RunCustomCommand { name: c.name.clone(), command: c.command.clone() })).collect(),
                        PanelId::Services => self.config.custom_commands.bulk_services.iter().map(|c| (c.name.clone(), DockerAction::RunCustomCommand { name: c.name.clone(), command: c.command.clone() })).collect(),
                        PanelId::Projects => self.config.custom_commands.bulk_projects.iter().map(|c| (c.name.clone(), DockerAction::RunCustomCommand { name: c.name.clone(), command: c.command.clone() })).collect(),
                        _ => Vec::new(),
                    };
                    items.extend(custom);
                }
                if let Some((title, items)) = menu {
                    self.state.enter_menu_mode(title, items);
                    self.ui.render.request_redraw();
                }
            }
            InputAction::ProjectLogs => {
                let panel = self.ui.focus.focused();
                let project_name = self.state.selected_project().map(|p| p.name.clone());
                let service_name = if panel == PanelId::Services {
                    self.state.selected_service_name()
                } else {
                    None
                };
                let tx = self.event_tx.clone();
                if let Some(project_name) = project_name
                    && let Some(tx) = tx
                {
                    self.state.set_main_context(MainContext::Logs);
                    self.stop_active_stream();
                    self.state.clear_log_buffer();
                    self.ui.render.request_redraw();

                    let compose = self.config.docker.compose_binary.clone();
                    let project_name_for_task = project_name.clone();
                    let service_name_for_task = service_name.clone();
                    let tx = tx.clone();
                    let handle = tokio::spawn(async move {
                        let parts: Vec<&str> = compose.split_whitespace().collect();
                        let mut cmd = if parts.is_empty() {
                            tokio::process::Command::new("docker")
                        } else {
                            tokio::process::Command::new(parts[0])
                        };
                        let mut args: Vec<&str> = Vec::new();
                        args.extend_from_slice(&parts[1..]);
                        args.extend_from_slice(&["-p", &project_name_for_task, "logs", "-f", "--no-color"]);
                        if let Some(ref service) = service_name_for_task {
                            args.push(service);
                        }
                        cmd.args(args);
                        cmd.stdout(std::process::Stdio::piped());
                        cmd.stderr(std::process::Stdio::null());
                        let mut child = match cmd.spawn() {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = tx.send(AppEvent::ActionResult(Err(format!(
                                    "Failed to start compose logs: {e}"
                                ))))
                                .await;
                                return;
                            }
                        };
                        let stdout = match child.stdout.take() {
                            Some(s) => s,
                            None => {
                                let _ = tx.send(AppEvent::ActionResult(Err(
                                    "Failed to capture compose logs stdout".to_string()
                                ))).await;
                                return;
                            }
                        };
                        let mut reader = tokio::io::BufReader::new(stdout).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            let chunk = crate::docker::LogChunk {
                                container_id: project_name_for_task.clone(),
                                stream: crate::docker::LogStream::Stdout,
                                bytes: line.into_bytes(),
                            };
                            if tx.send(AppEvent::LogChunk(chunk)).await.is_err() {
                                break;
                            }
                        }
                        let _ = child.kill().await;
                    });
                    self.active_stream = Some(ActiveStream {
                        container_id: project_name.clone(),
                        context: MainContext::Logs,
                        handle,
                    });
                }
            }
            InputAction::OpenInBrowser => {
                let panel = self.ui.focus.focused();
                let container_id = if panel == PanelId::Services {
                    if let (Some(project), Some(service)) = (
                        self.state.selected_project(),
                        self.state.selected_service_name()
                    ) {
                        self.state.container_for_service(&project.name, &service).map(|c| c.id.clone())
                    } else {
                        None
                    }
                } else {
                    self.state.selected_container().map(|c| c.id.clone())
                };
                if let Some(id) = container_id
                    && let Some(tx) = &self.event_tx
                {
                    let tx = tx.clone();
                    let Some(client) = self.supervisor.client() else {
                        self.state.set_error_message("Docker is not connected".to_string());
                        self.ui.render.request_redraw();
                        return;
                    };
                    tokio::spawn(async move {
                        match client.container_ports(id).await {
                            Ok(ports) if ports.is_empty() => {
                                let _ = tx.send(AppEvent::ActionResult(Err(
                                    "No exposed ports found".to_string()
                                ))).await;
                            }
                            Ok(ports) => {
                                let url = format!("http://localhost:{}", ports[0].1);
                                let result = open_browser(&url).await;
                                let _ = tx.send(AppEvent::ActionResult(result)).await;
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::ActionResult(Err(
                                    format!("Failed to get ports: {e}")
                                ))).await;
                            }
                        }
                    });
                }
            }
            InputAction::Confirm => {}
            InputAction::Cancel => {}
            InputAction::Redraw => self.ui.render.request_redraw(),
            InputAction::Leader => {}
        }
    }

    async fn handle_leader_key(&mut self, key: KeyStroke) {
        if key.code == crossterm::event::KeyCode::Esc {
            self.state.input_mode = InputMode::Normal;
            self.ui.render.request_redraw();
            return;
        }

        // Context-sensitive 'e' key: toggle hide stopped in containers panel
        if key.code == crossterm::event::KeyCode::Char('e')
            && self.ui.focus.focused() == PanelId::Containers
        {
            self.state.toggle_hide_stopped();
            self.ui.render.request_redraw();
            return;
        }

        // Context-sensitive 'e' key: edit compose config in projects panel
        if key.code == crossterm::event::KeyCode::Char('e')
            && self.ui.focus.focused() == PanelId::Projects
        {
            if let Some(project) = self.state.selected_project() {
                if let Some(dir) = &project.working_dir {
                    let path = format!("{dir}/docker-compose.yml");
                    self.state.external_command = Some(crate::state::ExternalCommand::EditConfig(path));
                    self.ui.render.request_redraw();
                } else {
                    self.state.set_status_message("No compose working directory known".to_string());
                }
            }
            return;
        }

        // Context-sensitive 'o' key: open compose config in projects panel
        if key.code == crossterm::event::KeyCode::Char('o')
            && self.ui.focus.focused() == PanelId::Projects
        {
            if let Some(project) = self.state.selected_project() {
                if let Some(dir) = &project.working_dir {
                    let path = format!("{dir}/docker-compose.yml");
                    self.state.external_command = Some(crate::state::ExternalCommand::OpenConfig(path));
                    self.ui.render.request_redraw();
                } else {
                    self.state.set_status_message("No compose working directory known".to_string());
                }
            }
            return;
        }

        // Context-sensitive 'p' key: pause/unpause container in containers panel
        if key.code == crossterm::event::KeyCode::Char('p')
            && self.ui.focus.focused() == PanelId::Containers
        {
            if let Some(container) = self.state.selected_container() {
                let id = container.id.clone();
                let action = if container.state.as_deref() == Some("paused") {
                    DockerAction::UnpauseContainer(id)
                } else {
                    DockerAction::PauseContainer(id)
                };
                if let Some(tx) = self.event_tx.as_ref() {
                    let _ = tx.send(AppEvent::ActionRequested(action)).await;
                }
            }
            return;
        }

        // Context-sensitive 'c' key: custom commands when a list panel is focused,
        // Config view when main panel is focused (handled by map_key below).
        if key.code == crossterm::event::KeyCode::Char('c')
            && self.ui.focus.focused() != PanelId::Main
        {
            let panel = self.ui.focus.focused();
            let (title, commands) = match panel {
                PanelId::Containers => {
                    let vars = self.state.selected_container().map(|c| TemplateVars {
                        container_id: Some(c.id.clone()),
                        container_name: c.names.first().cloned(),
                        ..Default::default()
                    }).unwrap_or_default();
                    let cmds: Vec<_> = self.config.custom_commands.containers.iter()
                        .map(|cmd| (cmd.name.clone(), cmd.render(&vars)))
                        .collect();
                    ("Container commands", cmds)
                }
                PanelId::Images => {
                    let vars = self.state.selected_image().map(|img| TemplateVars {
                        image_id: Some(img.id.clone()),
                        image_name: img.repo_tags.first().cloned(),
                        ..Default::default()
                    }).unwrap_or_default();
                    let cmds: Vec<_> = self.config.custom_commands.images.iter()
                        .map(|cmd| (cmd.name.clone(), cmd.render(&vars)))
                        .collect();
                    ("Image commands", cmds)
                }
                PanelId::Volumes => {
                    let vars = self.state.selected_volume().map(|vol| TemplateVars {
                        volume_name: Some(vol.name.clone()),
                        ..Default::default()
                    }).unwrap_or_default();
                    let cmds: Vec<_> = self.config.custom_commands.volumes.iter()
                        .map(|cmd| (cmd.name.clone(), cmd.render(&vars)))
                        .collect();
                    ("Volume commands", cmds)
                }
                PanelId::Networks => {
                    let vars = self.state.selected_network().map(|net| TemplateVars {
                        network_id: Some(net.id.clone()),
                        network_name: Some(net.name.clone()),
                        ..Default::default()
                    }).unwrap_or_default();
                    let cmds: Vec<_> = self.config.custom_commands.networks.iter()
                        .map(|cmd| (cmd.name.clone(), cmd.render(&vars)))
                        .collect();
                    ("Network commands", cmds)
                }
                PanelId::Services => {
                    let vars = self.state.selected_project().map(|p| TemplateVars {
                        project_name: Some(p.name.clone()),
                        ..Default::default()
                    }).unwrap_or_default();
                    let cmds: Vec<_> = self.config.custom_commands.services.iter()
                        .map(|cmd| (cmd.name.clone(), cmd.render(&vars)))
                        .collect();
                    ("Service commands", cmds)
                }
                PanelId::Projects => {
                    let vars = self.state.selected_project().map(|p| TemplateVars {
                        project_name: Some(p.name.clone()),
                        ..Default::default()
                    }).unwrap_or_default();
                    let cmds: Vec<_> = self.config.custom_commands.projects.iter()
                        .map(|cmd| (cmd.name.clone(), cmd.render(&vars)))
                        .collect();
                    ("Project commands", cmds)
                }
                PanelId::Main => ("Custom commands", Vec::new()),
                PanelId::Status => ("Custom commands", Vec::new()),
            };
            if !commands.is_empty() {
                let items: Vec<(String, DockerAction)> = commands.into_iter()
                    .map(|(name, command)| (name.clone(), DockerAction::RunCustomCommand { name, command }))
                    .collect();
                self.state.enter_menu_mode(title.to_string(), items);
                self.ui.render.request_redraw();
            } else {
                self.state.set_status_message("No custom commands configured".to_string());
                self.ui.render.request_redraw();
            }
            return;
        }

        // Context-sensitive 'S' key: start service in services panel,
        // stats view when main panel is focused (handled by map_key below).
        if key.code == crossterm::event::KeyCode::Char('S')
            && self.ui.focus.focused() == PanelId::Services
        {
            if let (Some(project), Some(service), Some(tx)) = (
                self.state.selected_project(),
                self.state.selected_service_name(),
                &self.event_tx
            ) {
                let _ = tx.send(AppEvent::ActionRequested(
                    DockerAction::ServiceStart { project: project.name.clone(), service }
                )).await;
            }
            return;
        }

        let action = map_key(key);
        if Self::is_sensitive(&action) {
            if let Some((docker_action, prompt)) = self.build_confirm_action(&action) {
                self.state.pending_leader_action = Some(action);
                self.state.enter_confirm_mode(docker_action, prompt);
                self.ui.render.request_redraw();
            }
            return;
        }

        self.dispatch_input_action(action).await;
    }

    fn is_sensitive(action: &InputAction) -> bool {
        matches!(
            action,
            InputAction::Prune | InputAction::ProjectDown
        )
    }

    fn build_confirm_action(&self, action: &InputAction) -> Option<(DockerAction, String)> {
        match action {
            InputAction::Prune => {
                match self.ui.focus.focused() {
                    PanelId::Images => Some((DockerAction::PruneImages, "Prune all unused images?".to_string())),
                    PanelId::Volumes => Some((DockerAction::PruneVolumes, "Prune all unused volumes?".to_string())),
                    PanelId::Networks => Some((DockerAction::PruneNetworks, "Prune all unused networks?".to_string())),
                    _ => None,
                }
            }
            InputAction::ProjectDown => {
                if let Some(project) = self.state.selected_project() {
                    return Some((
                        DockerAction::ProjectDown(project.name.clone()),
                        format!("Down project {}?", project.name),
                    ));
                }
                None
            }
            _ => None,
        }
    }

    fn handle_search_key(&mut self, key: KeyStroke) {
        match key.code {
            KeyCode::Esc => {
                self.state.exit_search_mode();
                self.ui.render.request_redraw();
            }
            KeyCode::Backspace => {
                self.state.backspace_search_char();
                self.ui.render.request_redraw();
            }
            KeyCode::Enter => {
                self.state.confirm_search_mode();
                self.ui.render.request_redraw();
            }
            KeyCode::Char(c) => {
                self.state.append_search_char(c);
                self.ui.render.request_redraw();
            }
            _ => {}
        }
    }

    async fn handle_mouse_event(&mut self, mouse: crate::ui::input::MouseEvent) {
        use crate::ui::input::MouseEventKind;

        match mouse.kind {
            MouseEventKind::Down => {
                if let Some(panel_id) = self.ui.panel_at(mouse.column, mouse.row)
                    && panel_id != PanelId::Status
                    && self.ui.focus.set_focus(panel_id)
                {
                    self.state.focused_panel = panel_id;
                    self.sync_compact_focus(panel_id);
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            MouseEventKind::ScrollUp => {
                if self.ui.focus.focused() == PanelId::Main {
                    self.state.scroll_main_up();
                    self.ui.render.request_redraw();
                } else if self.state.move_selection(self.ui.focus.focused(), -1) {
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            MouseEventKind::ScrollDown => {
                if self.ui.focus.focused() == PanelId::Main {
                    self.state.scroll_main_down();
                    self.ui.render.request_redraw();
                } else if self.state.move_selection(self.ui.focus.focused(), 1) {
                    self.update_active_stream().await;
                    self.ui.render.request_redraw();
                }
            }
            MouseEventKind::Up => {}
        }
    }

    async fn handle_confirm_key(&mut self, key: KeyStroke) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.state.exit_confirm_mode();
                self.ui.render.request_redraw();
            }
            KeyCode::Char('y') => {
                if let Some(action) = self.state.pending_leader_action.take() {
                    self.state.exit_confirm_mode();
                    self.dispatch_input_action(action).await;
                    self.ui.render.request_redraw();
                    return;
                }
                if let (Some((action, _)), Some(tx)) =
                    (self.state.pending_confirmation.take(), &self.event_tx)
                {
                    let _ = tx.send(AppEvent::ActionRequested(action)).await;
                }
                self.state.input_mode = InputMode::Normal;
                self.ui.render.request_redraw();
            }
            _ => {}
        }
    }

    async fn handle_menu_key(&mut self, key: KeyStroke) {
        match key.code {
            KeyCode::Esc => {
                self.state.exit_menu_mode();
                self.ui.render.request_redraw();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.state.move_menu_selection(1);
                self.ui.render.request_redraw();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.state.move_menu_selection(-1);
                self.ui.render.request_redraw();
            }
            KeyCode::Enter => {
                if let (Some(action), Some(tx)) =
                    (self.state.selected_menu_action().cloned(), &self.event_tx)
                {
                    let _ = tx.send(AppEvent::ActionRequested(action)).await;
                }
                self.state.exit_menu_mode();
                self.ui.render.request_redraw();
            }
            _ => {}
        }
    }

    fn open_options_menu(&mut self) {
        let panel = self.ui.focus.focused();
        let mut items: Vec<(String, DockerAction)> = Vec::new();

        match panel {
            PanelId::Containers => {
                if let Some(container) = self.state.selected_container() {
                    let id = container.id.clone();
                    let _name = container.names.first().cloned().unwrap_or_else(|| id.clone());

                    if container.state.as_deref() != Some("running") {
                        items.push(("Start".to_string(), DockerAction::StartContainer(id.clone())));
                    } else {
                        items.push(("Stop".to_string(), DockerAction::StopContainer(id.clone())));
                        items.push(("Restart".to_string(), DockerAction::RestartContainer(id.clone())));
                        if container.status.as_deref() == Some("paused") {
                            items.push(("Unpause".to_string(), DockerAction::UnpauseContainer(id.clone())));
                        } else {
                            items.push(("Pause".to_string(), DockerAction::PauseContainer(id.clone())));
                        }
                    }
                    items.push(("Exec shell".to_string(), DockerAction::ExecShell(id.clone())));
                    items.push(("Attach".to_string(), DockerAction::AttachContainer(id.clone())));
                    items.push(("Open in browser".to_string(), DockerAction::OpenInBrowser(id.clone())));
                    items.push(("Restart policy".to_string(), DockerAction::UpdateRestartPolicy {
                        id: id.clone(),
                        policy: "always".to_string(),
                    }));
                    items.push(("Remove".to_string(), DockerAction::RemoveContainer(id.clone())));
                    items.push(("Force remove".to_string(), DockerAction::ForceRemoveContainer(id.clone())));
                    items.push(("Remove with volumes".to_string(), DockerAction::RemoveContainerWithVolumes(id.clone())));
                }
            }
            PanelId::Services => {
                if let (Some(project), Some(service)) = (
                    self.state.selected_project(),
                    self.state.selected_service_name()
                ) {
                    let proj = project.name.clone();
                    let svc = service.clone();
                    items.push(("Up".to_string(), DockerAction::ServiceUp { project: proj.clone(), service: svc.clone() }));
                    items.push(("Start".to_string(), DockerAction::ServiceStart { project: proj.clone(), service: svc.clone() }));
                    items.push(("Stop".to_string(), DockerAction::ServiceStop { project: proj.clone(), service: svc.clone() }));
                    items.push(("Restart".to_string(), DockerAction::ServiceRestart { project: proj.clone(), service: svc.clone() }));
                    items.push(("Down".to_string(), DockerAction::ServiceDown { project: proj.clone(), service: svc.clone() }));

                    if let Some(container) = self.state.container_for_service(&proj, &svc) {
                        let id = container.id.clone();
                        items.push(("Exec shell".to_string(), DockerAction::ExecShell(id.clone())));
                        items.push(("Attach".to_string(), DockerAction::AttachContainer(id.clone())));
                        items.push(("Open in browser".to_string(), DockerAction::OpenInBrowser(id.clone())));
                    }
                }
            }
            PanelId::Projects => {
                if let Some(project) = self.state.selected_project() {
                    let name = project.name.clone();
                    items.push(("Up".to_string(), DockerAction::ProjectUp(name.clone())));
                    items.push(("Down".to_string(), DockerAction::ProjectDown(name.clone())));
                }
            }
            PanelId::Images => {
                if let Some(image) = self.state.selected_image() {
                    let id = image.id.clone();
                    items.push(("Delete".to_string(), DockerAction::DeleteImage(id.clone())));
                    items.push(("Force delete".to_string(), DockerAction::ForceDeleteImage(id.clone())));
                    items.push(("Prune unused images".to_string(), DockerAction::PruneImages));
                }
            }
            PanelId::Volumes => {
                if let Some(volume) = self.state.selected_volume() {
                    let name = volume.name.clone();
                    items.push(("Delete".to_string(), DockerAction::DeleteVolume(name.clone())));
                    items.push(("Prune unused volumes".to_string(), DockerAction::PruneVolumes));
                }
            }
            PanelId::Networks => {
                if let Some(network) = self.state.selected_network() {
                    let id = network.id.clone();
                    items.push(("Delete".to_string(), DockerAction::DeleteNetwork(id.clone())));
                    items.push(("Prune unused networks".to_string(), DockerAction::PruneNetworks));
                }
            }
            _ => {}
        }

        if items.is_empty() {
            self.state.set_status_message("No options available".to_string());
        } else {
            self.state.enter_menu_mode(format!("{panel:?} options"), items);
        }
    }

    fn sync_compact_focus(&mut self, panel_id: PanelId) {
        let sidebar_panels = [
            PanelId::Projects,
            PanelId::Services,
            PanelId::Containers,
            PanelId::Images,
            PanelId::Volumes,
            PanelId::Networks,
        ];
        if sidebar_panels.contains(&panel_id) {
            self.state.compact_sidebar_panel = panel_id;
            self.ui.invalidate_layout();
            if self.is_compact_mode() {
                self.ui.focus.set_order(&[panel_id, PanelId::Main]);
            }
        }

        // Auto-switch main context based on focused panel
        match panel_id {
            PanelId::Images => {
                self.state.set_main_context(crate::state::MainContext::ImageInfo);
            }
            PanelId::Volumes => {
                self.state.set_main_context(crate::state::MainContext::VolumeInfo);
            }
            PanelId::Networks => {
                self.state.set_main_context(crate::state::MainContext::NetworkInfo);
            }
            PanelId::Containers | PanelId::Services | PanelId::Projects => {
                if matches!(
                    self.state.active_main_context,
                    crate::state::MainContext::ImageInfo
                        | crate::state::MainContext::VolumeInfo
                        | crate::state::MainContext::NetworkInfo
                ) {
                    self.state.set_main_context(crate::state::MainContext::Logs);
                }
            }
            _ => {}
        }
    }

    fn is_compact_mode(&self) -> bool {
        let (w, h) = self.state.terminal_size;
        w > 0 && w < self.state.compact_mode_width && h >= 6
    }

    fn sync_focus_order_for_layout(&mut self) {
        if self.is_compact_mode() {
            self.ui.focus.set_order(&[
                self.state.compact_sidebar_panel,
                PanelId::Main,
            ]);
        } else {
            self.ui.focus.set_order(&[
                PanelId::Projects,
                PanelId::Services,
                PanelId::Containers,
                PanelId::Images,
                PanelId::Volumes,
                PanelId::Networks,
                PanelId::Main,
            ]);
        }
    }

    async fn update_active_stream(&mut self) {
        let Some(container) = self.state.selected_container() else {
            self.stop_active_stream();
            return;
        };

        let container_id = container.id.clone();
        let context = self.state.active_main_context;

        if let Some(stream) = &self.active_stream
            && stream.container_id == container_id && stream.context == context {
                return;
            }

        let container_changed = self
            .active_stream
            .as_ref()
            .is_some_and(|s| s.container_id != container_id);

        self.stop_active_stream();

        if container_changed {
            self.state.env_vars.clear();
            self.state.main_scroll_offsets.clear();
        }

        let Some(tx) = &self.event_tx else { return };
        let tx = tx.clone();

        match context {
            MainContext::Logs => {
                match self.supervisor.stream_logs(&container_id).await {
                    Ok(mut logs_rx) => {
                        let handle = tokio::spawn(async move {
                            while let Some(chunk) = logs_rx.recv().await {
                                if tx.send(AppEvent::LogChunk(chunk)).await.is_err() {
                                    break;
                                }
                            }
                        });
                        self.active_stream = Some(ActiveStream {
                            container_id,
                            context,
                            handle,
                        });
                    }
                    Err(err) => {
                        self.state
                            .set_error_message(format!("Failed to stream logs: {err}"));
                    }
                }
            }
            MainContext::Stats => {
                match self.supervisor.stream_stats(&container_id).await {
                    Ok(mut stats_rx) => {
                        let handle = tokio::spawn(async move {
                            while let Some(sample) = stats_rx.recv().await {
                                if tx.send(AppEvent::StatsSample(sample)).await.is_err() {
                                    break;
                                }
                            }
                        });
                        self.active_stream = Some(ActiveStream {
                            container_id,
                            context,
                            handle,
                        });
                    }
                    Err(err) => {
                        self.state
                            .set_error_message(format!("Failed to stream stats: {err}"));
                    }
                }
            }
            MainContext::Config => {}
            MainContext::ImageInfo | MainContext::VolumeInfo | MainContext::NetworkInfo => {}
            MainContext::Env => {
                let tx = tx.clone();
                let Some(client) = self.supervisor.client() else {
                    self.state
                        .set_error_message("Docker is not connected".to_string());
                    self.ui.render.request_redraw();
                    return;
                };
                let env_container_id = container_id.clone();
                let handle = tokio::spawn(async move {
                    match client.container_env(env_container_id).await {
                        Ok(vars) => {
                            let _ = tx.send(AppEvent::EnvVarsLoaded(vars)).await;
                        }
                        Err(err) => {
                            let _ = tx.send(AppEvent::EnvVarsFailed(err.to_string())).await;
                        }
                    }
                });
                self.active_stream = Some(ActiveStream {
                    container_id,
                    context,
                    handle,
                });
            }
        }
    }

    fn stop_active_stream(&mut self) {
        if let Some(stream) = self.active_stream.take() {
            stream.handle.abort();
            self.state.clear_log_buffer();
            self.state.clear_stats();
        }
    }

    async fn run_external_command(
        &mut self,
        terminal: &mut TerminalSession,
        cmd: crate::state::ExternalCommand,
    ) -> AppResult<()> {
        self.stop_active_stream();
        terminal.suspend()?;

        let result = match cmd {
            crate::state::ExternalCommand::ExecShell(id) => {
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new("docker")
                        .args(["exec", "-it", &id, "sh"])
                        .status()
                })
                .await
            }
            crate::state::ExternalCommand::AttachContainer(id) => {
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new("docker")
                        .args(["attach", &id])
                        .status()
                })
                .await
            }
            crate::state::ExternalCommand::CustomCommand { command, .. } => {
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new("sh")
                        .args(["-c", &command])
                        .status()
                })
                .await
            }
            crate::state::ExternalCommand::EditConfig(path) => {
                let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new(&editor)
                        .arg(&path)
                        .status()
                })
                .await
            }
            crate::state::ExternalCommand::OpenConfig(path) => {
                #[cfg(target_os = "macos")]
                let opener = "open";
                #[cfg(target_os = "linux")]
                let opener = "xdg-open";
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                let opener = "xdg-open";
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new(opener)
                        .arg(&path)
                        .status()
                })
                .await
            }
        };

        terminal.resume(&self.config.terminal)?;

        match result {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(AppError::Generic(format!(
                "command exited with code {}",
                status.code().unwrap_or(-1)
            ))),
            Ok(Err(err)) => Err(AppError::Generic(format!("failed to run command: {err}"))),
            Err(err) => Err(AppError::Generic(format!("command task panicked: {err}"))),
        }
    }

    async fn shutdown_tasks(&self, mut tasks: JoinSet<()>) {
        tasks.abort_all();

        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(()) => {}
                Err(err) if err.is_cancelled() => {}
                Err(err) => error!(%err, "background task failed during shutdown"),
            }
        }
    }
}

fn read_input_loop(tx: tokio::sync::mpsc::Sender<AppEvent>, cancel: CancellationToken) {
    while !cancel.is_cancelled() && !tx.is_closed() {
        match read_terminal_input(Duration::from_millis(50)) {
            Ok(Some(input)) => {
                if tx.blocking_send(AppEvent::Input(input)).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(err) => {
                let _ = tx.blocking_send(AppEvent::ShutdownRequested(ShutdownReason::FatalError(
                    format!("failed to read terminal input: {err}"),
                )));
                break;
            }
        }
    }
}

/// Strip ANSI escape sequences and control characters from a string to prevent
/// terminal corruption when rendering container logs inside a TUI.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    // CSI sequence: ESC [ ... final-byte
                    chars.next(); // consume '['
                    while chars.peek().is_some_and(|&c| ('0'..='?').contains(&c)) {
                        chars.next();
                    }
                    while chars.peek().is_some_and(|&c| (' '..='/').contains(&c)) {
                        chars.next();
                    }
                    if chars.peek().is_some_and(|&c| ('@'..='~').contains(&c)) {
                        chars.next();
                    }
                }
                Some(&']') => {
                    // OSC sequence: ESC ] ... BEL or ESC \
                    chars.next(); // consume ']'
                    while let Some(&c) = chars.peek() {
                        if c == '\x07' {
                            chars.next();
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                Some(&'(') | Some(&')') | Some(&'*') | Some(&'+') => {
                    // Character set sequences: ESC ( B, ESC ) B, etc.
                    chars.next();
                    if chars.peek().is_some_and(|&c| (' '..='~').contains(&c)) {
                        chars.next();
                    }
                }
                Some(&'%') => {
                    // UTF-8 selection: ESC % G or ESC % @
                    chars.next();
                    if chars.peek().is_some_and(|&c| c == '@' || c == 'G') {
                        chars.next();
                    }
                }
                Some(&'c') => {
                    // RIS — full terminal reset. Drop both bytes.
                    chars.next();
                }
                Some(&'#') => {
                    // DEC sequences: ESC # 3, ESC # 4, ESC # 5, ESC # 6, ESC # 8
                    chars.next();
                    if chars.peek().is_some() {
                        chars.next();
                    }
                }
                Some(&'P') | Some(&'_') | Some(&'^') => {
                    // DCS, APC, PM sequences: ESC P/_{...} ESC \ or BEL
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        if c == '\x07' {
                            chars.next();
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                Some(&'N') | Some(&'O') | Some(&'=') | Some(&'>') | Some(&'<') => {
                    // SS2, SS3, keypad modes
                    chars.next();
                }
                Some(&c) if ('0'..='~').contains(&c) => {
                    // Single-byte sequences: ESC 7 (save cursor), ESC 8 (restore), etc.
                    chars.next();
                }
                _ => {
                    // Lone ESC - drop it to be safe
                }
            }
        } else if ch == '\r' || ch == '\x08' || ch == '\x0b' || ch == '\x0c' {
            // Drop carriage return, backspace, vertical tab, form feed
            // These can cause cursor movement or overwrites in terminals
        } else if ch.is_control() && ch != '\n' && ch != '\t' {
            // Drop other control chars except newline and tab
            // (tabs are expanded later; newlines are preserved for splitting)
        } else {
            result.push(ch);
        }
    }
    result
}

async fn run_compose(compose_binary: &str, project: &str, args: &[&str]) -> Result<(), String> {
    let parts: Vec<&str> = compose_binary.split_whitespace().collect();
    if parts.is_empty() {
        return Err("compose_binary is empty".to_string());
    }
    let mut cmd = tokio::process::Command::new(parts[0]);
    cmd.args(&parts[1..]);
    cmd.arg("-p").arg(project);
    cmd.args(args);
    let output = cmd.output().await.map_err(|e| format!("failed to run compose: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let code = output.status.code().unwrap_or(-1);
        let msg = if stderr.trim().is_empty() {
            if stdout.trim().is_empty() {
                format!("compose exited with code {code}")
            } else {
                format!("compose exited with code {code}: {stdout}")
            }
        } else {
            format!("compose failed: {stderr}")
        };
        Err(msg)
    }
}

async fn open_browser(url: &str) -> Result<(), String> {
    let url_lower = url.to_lowercase();
    if !url_lower.starts_with("http://") && !url_lower.starts_with("https://") {
        return Err(format!("Refusing to open non-HTTP URL: {url}"));
    }
    let commands = ["xdg-open", "open"];
    for cmd in commands {
        match tokio::process::Command::new(cmd).arg(url).status().await {
            Ok(status) if status.success() => return Ok(()),
            _ => continue,
        }
    }
    Err("Failed to open browser. Install xdg-open (Linux) or ensure 'open' is available (macOS).".to_string())
}

#[cfg(test)]
mod tests {
    use crate::{
        docker::{BollardDockerClient, ContainerItem, DockerInfo, DockerUpdate},
        events::ShutdownReason,
        ui::input::{InputEvent, KeyStroke},
    };

    use super::*;

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        assert_eq!(strip_ansi("hello"), "hello");
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("\x1b[1;31mbold red\x1b[0m"), "bold red");
        assert_eq!(strip_ansi("\x1b[2K\x1b[Gclear line"), "clear line");
        assert_eq!(strip_ansi("\x1b(Bnormal text"), "normal text");
        assert_eq!(
            strip_ansi("before\x1b[31mred\x1b[0mafter"),
            "beforeredafter"
        );
        // OSC sequences
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
        assert_eq!(strip_ansi("\x1b]7;file://host\x1b\\text"), "text");
        // Control characters
        assert_eq!(strip_ansi("hello\rworld"), "helloworld");
        assert_eq!(strip_ansi("hello\x08world"), "helloworld");
        assert_eq!(strip_ansi("hello\x0bworld"), "helloworld");
        assert_eq!(strip_ansi("hello\x0cworld"), "helloworld");
        // Terminal reset / dangerous sequences
        assert_eq!(strip_ansi("before\x1bcclear"), "beforeclear");
        assert_eq!(strip_ansi("text\x1b#8more"), "textmore");
        // DCS sequences
        assert_eq!(strip_ansi("\x1bP1;2|data\x1b\\after"), "after");
        // APC sequences
        assert_eq!(strip_ansi("\x1b_tmux\x07after"), "after");
        // PM sequences
        assert_eq!(strip_ansi("\x1b^priv\x1b\\after"), "after");
        // SS2 / SS3 / keypad modes
        assert_eq!(strip_ansi("\x1bNafter"), "after");
        assert_eq!(strip_ansi("\x1b=after"), "after");
        // Preserve newlines and tabs
        assert_eq!(strip_ansi("hello\nworld"), "hello\nworld");
        assert_eq!(strip_ansi("hello\tworld"), "hello\tworld");
    }

    #[test]
    fn strip_ansi_never_panics_and_drops_escapes() {
        // Stress-test with random-ish inputs containing ANSI sequences,
        // embedded nulls, invalid UTF-8 sequences, and control chars.
        let inputs = [
            "\x1b[31m\x1b[0m\x1b[2K",
            "\x1b]0;\x07\x1b\\",
            "\x1bP\x1b\\",
            "\x1b_c\x07",
            "\x1b#8",
            "\x1bN\x1bO\x1b=\x1b>\x1b<",
            "normal\x1b[1mbold\x1b[0mnormal",
            "\x1b[1;2;3;4;5;6;7;8;9m",
            "\x1b[\x1b[",
            "\x1b",
            "\x1b\x1b",
            "[)",
            "\x1b[\n",
            "a\x00b\x1bc\x7fd",
            "emoji 🎉\x1b[31m🔥\x1b[0m",
            "\x1b]7;file://host/path\x1b\\after",
            // DCS with no terminator (should consume rest or drop)
            "before\x1bPdata",
            // OSC with no terminator
            "before\x1b]data",
            // Nested/combined sequences
            "\x1b[31m\x1b]0;title\x07\x1b[0m",
        ];

        for input in &inputs {
            let result = strip_ansi(input);
            assert!(
                !result.contains('\x1b'),
                "strip_ansi left ESC in output for input {:?}: {:?}",
                input,
                result
            );
        }
    }

    #[tokio::test]
    async fn app_starts_without_docker_client() {
        let config = AppConfig {
            docker: crate::config::DockerConfig {
                ping_on_startup: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let supervisor = DockerSupervisor::new(None, DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_event(AppEvent::Tick).await.unwrap();

        // Should not panic and should report Docker as unavailable
        assert_eq!(app.state.tick_count, 1);
        assert!(app.supervisor.client().is_none());
    }

    #[tokio::test]
    async fn event_loop_updates_state_until_shutdown() {
        let config = AppConfig {
            docker: crate::config::DockerConfig {
                ping_on_startup: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_event(AppEvent::Tick).await.unwrap();
        app.handle_event(AppEvent::DockerPinged(Ok(DockerInfo {
            server_version: Some("test".to_string()),
        })))
        .await
        .unwrap();
        app.handle_event(AppEvent::ShutdownRequested(ShutdownReason::User))
            .await
            .unwrap();

        assert_eq!(app.state.tick_count, 1);
        assert!(!app.state.running);
        assert_eq!(app.state.last_shutdown_reason, Some(ShutdownReason::User));
    }

    #[tokio::test]
    async fn rapid_navigation_marks_renderer_dirty_without_touching_docker() {
        let config = AppConfig {
            docker: crate::config::DockerConfig {
                ping_on_startup: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.ui
            .render
            .finish_render_for_test(ratatui::layout::Rect::new(0, 0, 80, 24));
        assert!(!app.ui.render.should_render());

        for _ in 0..100 {
            app.handle_input(InputEvent::Key(KeyStroke {
                code: crossterm::event::KeyCode::Char(']'),
                modifiers: crossterm::event::KeyModifiers::NONE,
            })).await;
        }

        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn docker_updates_propagate_to_state() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        let container = ContainerItem {
            id: "test-id".to_string(),
            names: vec!["test-container".to_string()],
            image: "test-image".to_string(),
            state: Some("running".to_string()),
            status: Some("Up 2 minutes".to_string()),
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        };

        app.handle_event(AppEvent::DockerUpdate(DockerUpdate::Containers(vec![
            container.clone(),
        ])))
        .await
        .unwrap();

        assert_eq!(app.state.containers.len(), 1);
        assert_eq!(app.state.containers[0].id, "test-id");
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn env_vars_loaded_updates_state_and_marks_dirty() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.ui
            .render
            .finish_render_for_test(ratatui::layout::Rect::new(0, 0, 80, 24));
        assert!(!app.ui.render.should_render());

        app.handle_event(AppEvent::EnvVarsLoaded(vec![
            ("FOO".to_string(), "bar".to_string()),
        ]))
        .await
        .unwrap();

        assert_eq!(app.state.env_vars.len(), 1);
        assert_eq!(app.state.env_vars[0], ("FOO".to_string(), "bar".to_string()));
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn action_failure_sets_persistent_error_message() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_event(AppEvent::ActionResult(Err("network timeout".to_string())))
            .await
            .unwrap();

        assert_eq!(
            app.state.error_message,
            Some("Action failed: network timeout".to_string())
        );
        assert!(app.state.status_message.is_none());
    }

    #[tokio::test]
    async fn new_action_clears_previous_error() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_event(AppEvent::ActionResult(Err("boom".to_string())))
            .await
            .unwrap();
        assert!(app.state.error_message.is_some());

        app.handle_event(AppEvent::ActionRequested(DockerAction::PruneImages))
            .await
            .unwrap();
        assert!(app.state.error_message.is_none());
    }

    #[tokio::test]
    async fn search_mode_filters_logs_and_exits_on_escape() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        use crate::docker::LogStream;
        app.state
            .log_buffer
            .push_back(("hello world".to_string(), LogStream::Stdout));
        app.state
            .log_buffer
            .push_back(("error: something broke".to_string(), LogStream::Stderr));
        app.state
            .log_buffer
            .push_back(("goodbye world".to_string(), LogStream::Stdout));

        app.ui
            .render
            .finish_render_for_test(ratatui::layout::Rect::new(0, 0, 80, 24));
        assert!(!app.ui.render.should_render());

        // Focus main panel so / searches logs
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Main);

        // Enter search mode
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('/'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        assert_eq!(app.state.input_mode, crate::state::InputMode::Search);
        assert!(app.ui.render.should_render());

        // Type filter
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('e'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        assert_eq!(app.state.log_filter, Some("e".to_string()));

        // Confirm search
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Enter,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
        assert_eq!(app.state.log_filter, Some("e".to_string()));

        // Exit clears filter
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('/'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Esc,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
        assert!(app.state.log_filter.is_none());
    }

    #[tokio::test]
    async fn env_vars_failed_sets_error_message() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_event(AppEvent::EnvVarsFailed("container not running".to_string()))
            .await
            .unwrap();

        assert_eq!(
            app.state.error_message,
            Some("Failed to load env vars: container not running".to_string())
        );
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn delete_selected_enters_confirm_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: None,
            status: None,
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('d'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Menu);
        assert!(!app.state.menu_items.is_empty());
        assert!(app.state.menu_title.contains("Remove container web"));
        assert_eq!(app.state.menu_items[0], "Remove");
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn cancel_deletion_exits_confirm_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.enter_confirm_mode(
            crate::events::DockerAction::RemoveContainer("x".to_string()),
            "test?",
        );

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('n'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
        assert!(app.state.pending_confirmation.is_none());
    }

    #[tokio::test]
    async fn esc_cancels_confirmation() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.enter_confirm_mode(
            crate::events::DockerAction::RemoveContainer("x".to_string()),
            "test?",
        );

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Esc,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
        assert!(app.state.pending_confirmation.is_none());
    }

    #[tokio::test]
    async fn prune_images_enters_confirm_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.ui.focus.set_focus(crate::ui::panel::PanelId::Images);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('p'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Confirm);
        assert!(app.state.pending_confirmation.is_some());
        let (action, prompt) = app.state.pending_confirmation.as_ref().unwrap();
        assert!(matches!(action, crate::events::DockerAction::PruneImages));
        assert!(prompt.contains("Prune all unused images"));
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn prune_volumes_enters_confirm_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.ui.focus.set_focus(crate::ui::panel::PanelId::Volumes);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('p'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Confirm);
        assert!(app.state.pending_confirmation.is_some());
        let (action, prompt) = app.state.pending_confirmation.as_ref().unwrap();
        assert!(matches!(action, crate::events::DockerAction::PruneVolumes));
        assert!(prompt.contains("Prune all unused volumes"));
    }

    #[tokio::test]
    async fn prune_on_unsupported_panel_does_nothing() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('p'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
        assert!(app.state.pending_confirmation.is_none());
    }

    #[tokio::test]
    async fn p_key_on_containers_panel_issues_pause_for_running_container() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        let (bus, mut rx) = EventBus::new(8);
        app.event_tx = Some(bus.publisher());

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc123".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: Some("running".to_string()),
            status: None,
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('p'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        let event = rx.recv().await;
        assert!(matches!(
            event,
            Some(AppEvent::ActionRequested(DockerAction::PauseContainer(id))) if id == "abc123"
        ));
    }

    #[tokio::test]
    async fn p_key_on_containers_panel_issues_unpause_for_paused_container() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        let (bus, mut rx) = EventBus::new(8);
        app.event_tx = Some(bus.publisher());

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc123".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: Some("paused".to_string()),
            status: None,
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('p'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        let event = rx.recv().await;
        assert!(matches!(
            event,
            Some(AppEvent::ActionRequested(DockerAction::UnpauseContainer(id))) if id == "abc123"
        ));
    }

    #[tokio::test]
    async fn show_help_enters_help_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('h'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Help);
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn any_key_exits_help_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.enter_help_mode();

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('x'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
        assert!(app.ui.render.should_render());
    }

    #[tokio::test]
    async fn question_mark_enters_help_mode() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('?'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Help);
    }

    #[tokio::test]
    async fn exec_shell_sets_pending_flag() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: None,
            status: None,
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_event(AppEvent::ActionRequested(crate::events::DockerAction::ExecShell(
            "abc".to_string(),
        )))
        .await
        .unwrap();

        assert_eq!(
            app.state.external_command,
            Some(crate::state::ExternalCommand::ExecShell("abc".to_string()))
        );
    }

    #[tokio::test]
    async fn attach_container_sets_pending_flag() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: None,
            status: None,
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_event(AppEvent::ActionRequested(crate::events::DockerAction::AttachContainer(
            "abc".to_string(),
        )))
        .await
        .unwrap();

        assert_eq!(
            app.state.external_command,
            Some(crate::state::ExternalCommand::AttachContainer("abc".to_string()))
        );
    }

    #[tokio::test]
    async fn quit_with_confirm_on_quit_enters_confirm_mode() {
        let mut config = AppConfig::default();
        config.confirm_on_quit = true;
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('q'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Confirm);
        assert!(app.state.pending_confirmation.is_some());
        let (action, prompt) = app.state.pending_confirmation.as_ref().unwrap();
        assert!(matches!(action, DockerAction::Quit));
        assert!(prompt.contains("quit"));
    }

    #[tokio::test]
    async fn quit_without_confirm_on_quit_requests_shutdown() {
        let mut config = AppConfig::default();
        config.confirm_on_quit = false;
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('q'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert!(!app.state.running);
    }

    #[tokio::test]
    async fn confirm_quit_requests_shutdown() {
        let mut config = AppConfig::default();
        config.confirm_on_quit = true;
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        let (bus, mut rx) = EventBus::new(8);
        app.event_tx = Some(bus.publisher());

        app.state.enter_confirm_mode(DockerAction::Quit, "Quit?".to_string());

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('y'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        let event = rx.recv().await;
        assert!(matches!(event, Some(AppEvent::ActionRequested(DockerAction::Quit))));

        app.handle_event(event.unwrap()).await.unwrap();

        assert!(!app.state.running);
        assert_eq!(app.state.input_mode, crate::state::InputMode::Normal);
    }

    #[tokio::test]
    async fn options_menu_opens_for_containers_panel() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: Some("running".to_string()),
            status: Some("Up 2 minutes".to_string()),
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.state.selected_indexes.insert(crate::ui::panel::PanelId::Containers, 0);
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('x'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert_eq!(app.state.input_mode, crate::state::InputMode::Menu);
        assert!(!app.state.menu_items.is_empty());
        assert!(app.state.menu_title.contains("Containers"));
    }

    #[tokio::test]
    async fn options_menu_shows_start_for_stopped_container() {
        let config = AppConfig::default();
        let client = BollardDockerClient::from_docker(
            bollard::Docker::connect_with_local_defaults().unwrap(),
            Duration::from_secs(5),
        );
        let supervisor = DockerSupervisor::new(Some(client), DockerRuntimeConfig::default());
        let mut app = App::with_supervisor(config, supervisor);

        app.state.containers = vec![crate::docker::ContainerItem {
            id: "abc".to_string(),
            names: vec!["web".to_string()],
            image: "nginx".to_string(),
            state: Some("exited".to_string()),
            status: Some("Exited".to_string()),
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        app.state.selected_indexes.insert(crate::ui::panel::PanelId::Containers, 0);
        app.ui.focus.set_focus(crate::ui::panel::PanelId::Containers);

        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char(' '),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;
        app.handle_input(InputEvent::Key(KeyStroke {
            code: KeyCode::Char('x'),
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .await;

        assert!(app.state.menu_items.iter().any(|i| i == "Start"));
        assert!(!app.state.menu_items.iter().any(|i| i == "Stop"));
    }
}
