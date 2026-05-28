use tokio::sync::mpsc;

use crate::docker::{ContainerStatsSample, DockerInfo, DockerUpdate, LogChunk};
use crate::ui::input::{InputAction, InputEvent};

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Started,
    Tick,
    Input(InputEvent),
    DockerPinged(Result<DockerInfo, String>),
    DockerUpdate(DockerUpdate),
    LogChunk(LogChunk),
    StatsSample(ContainerStatsSample),
    EnvVarsLoaded(Vec<(String, String)>),
    EnvVarsFailed(String),
    ActionRequested(DockerAction),
    ActionResult(Result<(), String>),
    ExecuteInputAction(InputAction),
    ShutdownRequested(ShutdownReason),
    ShutdownComplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerAction {
    StartContainer(String),
    StopContainer(String),
    RestartContainer(String),
    RemoveContainer(String),
    ForceRemoveContainer(String),
    RemoveContainerWithVolumes(String),
    PauseContainer(String),
    UnpauseContainer(String),
    DeleteImage(String),
    ForceDeleteImage(String),
    DeleteVolume(String),
    DeleteNetwork(String),
    PruneImages,
    PruneVolumes,
    PruneNetworks,
    ExecShell(String),
    AttachContainer(String),
    RunCustomCommand { name: String, command: String },
    ProjectUp(String),
    ProjectDown(String),
    ServiceUp { project: String, service: String },
    ServiceStart { project: String, service: String },
    ServiceStop { project: String, service: String },
    ServiceRestart { project: String, service: String },
    ServiceDown { project: String, service: String },
    BulkStopContainers,
    BulkStartContainers,
    BulkRestartContainers,
    BulkRemoveStoppedContainers,
    BulkProjectUp,
    BulkProjectDown,
    BulkServiceUp(String),
    BulkServiceStop(String),
    BulkServiceRestart(String),
    OpenInBrowser(String),
    UpdateRestartPolicy { id: String, policy: String },
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownReason {
    CtrlC,
    User,
    FatalError(String),
}

#[derive(Debug, Clone)]
pub struct EventBus {
    tx: mpsc::Sender<AppEvent>,
}

pub type EventReceiver = mpsc::Receiver<AppEvent>;

impl EventBus {
    pub fn new(buffer: usize) -> (Self, EventReceiver) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self { tx }, rx)
    }

    pub async fn publish(&self, event: AppEvent) -> Result<(), mpsc::error::SendError<AppEvent>> {
        self.tx.send(event).await
    }

    pub fn publisher(&self) -> mpsc::Sender<AppEvent> {
        self.tx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publishes_events_in_order() {
        let (bus, mut rx) = EventBus::new(8);
        bus.publish(AppEvent::Started).await.unwrap();
        bus.publish(AppEvent::Tick).await.unwrap();

        assert_eq!(rx.recv().await, Some(AppEvent::Started));
        assert_eq!(rx.recv().await, Some(AppEvent::Tick));
    }
}
