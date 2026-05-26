use std::time::Duration;

use tokio::{task::JoinSet, time};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    config::AppConfig,
    docker::{BollardDockerClient, DockerClient, DockerRuntimeConfig, DockerSupervisor},
    errors::{AppError, AppResult},
    events::{AppEvent, EventBus, EventReceiver, ShutdownReason},
    state::{AppState, MainContext},
    terminal::TerminalSession,
    ui::{
        AppUi,
        input::{InputAction, InputEvent, map_key, read_terminal_input},
        renderer,
    },
};

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

    async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl App {
    pub fn new(config: AppConfig) -> AppResult<Self> {
        let docker_config = DockerRuntimeConfig::default();
        let client = BollardDockerClient::connect_with_local_defaults(docker_config.request_timeout)?;
        let supervisor = DockerSupervisor::new(client, docker_config);
        Ok(Self::with_supervisor(config, supervisor))
    }

    pub fn with_supervisor(config: AppConfig, supervisor: DockerSupervisor) -> Self {
        Self {
            config,
            supervisor,
            state: AppState::default(),
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

        let loop_result = self.event_loop(rx, &mut terminal).await;

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

        if self.config.docker.ping_on_startup {
            let docker_tx = bus.publisher();
            let docker = self.supervisor.client();
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
    ) -> AppResult<()> {
        info!("app event loop started");
        self.handle_event(AppEvent::Started).await?;
        renderer::draw(terminal.terminal_mut(), &mut self.ui, &self.state)?;

        while self.state.running {
            match rx.recv().await {
                Some(event) => {
                    self.handle_event(event).await?;
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
            AppEvent::LogChunk(chunk) => {
                if let Some(stream) = &self.active_stream {
                    if stream.container_id == chunk.container_id && stream.context == MainContext::Logs {
                        let line = String::from_utf8_lossy(&chunk.bytes).to_string();
                        for l in line.lines() {
                            self.state.add_log_line(l.to_string());
                        }
                        self.ui.render.request_redraw();
                    }
                }
            }
            AppEvent::StatsSample(sample) => {
                if let Some(stream) = &self.active_stream {
                    if stream.container_id == sample.container_id && stream.context == MainContext::Stats {
                        self.state.update_stats(sample);
                        self.ui.render.request_redraw();
                    }
                }
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
            InputEvent::Key(key) => match map_key(key) {
                InputAction::None => {}
                InputAction::Quit => self.state.request_shutdown(ShutdownReason::User),
                InputAction::FocusNext => {
                    if self.ui.focus.focus_next() {
                        self.update_active_stream().await;
                        self.ui.render.request_redraw();
                    }
                }
                InputAction::FocusPrevious => {
                    if self.ui.focus.focus_previous() {
                        self.update_active_stream().await;
                        self.ui.render.request_redraw();
                    }
                }
                InputAction::FocusPanel(panel_id) => {
                    if self.ui.focus.set_focus(panel_id) {
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
                InputAction::Redraw => self.ui.render.request_redraw(),
            },
            InputEvent::Resize { .. } => self.ui.render.mark_resize(),
        }
    }

    async fn update_active_stream(&mut self) {
        let Some(container) = self.state.selected_container() else {
            self.stop_active_stream();
            return;
        };

        let container_id = container.id.clone();
        let context = self.state.active_main_context;

        if let Some(stream) = &self.active_stream {
            if stream.container_id == container_id && stream.context == context {
                return;
            }
        }

        self.stop_active_stream();

        let Some(tx) = &self.event_tx else { return };
        let tx = tx.clone();

        match context {
            MainContext::Logs => {
                if let Ok(mut logs_rx) = self.supervisor.stream_logs(&container_id).await {
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
            }
            MainContext::Stats => {
                if let Ok(mut stats_rx) = self.supervisor.stream_stats(&container_id).await {
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
            }
            _ => {}
        }
    }

    fn stop_active_stream(&mut self) {
        if let Some(stream) = self.active_stream.take() {
            stream.handle.abort();
            self.state.clear_log_buffer();
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

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin};

    use crate::{
        docker::{ComposeProject, ContainerItem, DockerError, DockerInfo, DockerUpdate},
        events::ShutdownReason,
        ui::input::{InputEvent, KeyStroke},
    };

    use super::*;

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
        let supervisor = DockerSupervisor::new(client, DockerRuntimeConfig::default());
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
        let supervisor = DockerSupervisor::new(client, DockerRuntimeConfig::default());
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
        let supervisor = DockerSupervisor::new(client, DockerRuntimeConfig::default());
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
}
