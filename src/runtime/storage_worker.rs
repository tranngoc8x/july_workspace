use super::RuntimeError;
use crate::domain::{
    AgentId, ConversationId, PermissionDecision, SessionBinding, SessionBindingId,
    SessionBindingStatus,
};
use crate::storage::{SqliteStore, StoreError};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use tokio::sync::{mpsc, oneshot};

const STORAGE_CAPACITY: usize = 64;

enum Command {
    InsertBinding(SessionBinding, oneshot::Sender<Result<(), StoreError>>),
    GetCurrentBinding(
        ConversationId,
        AgentId,
        oneshot::Sender<Result<Option<SessionBinding>, StoreError>>,
    ),
    ListCurrentBindings(
        AgentId,
        oneshot::Sender<Result<Vec<SessionBinding>, StoreError>>,
    ),
    UpdateBindingStatus(
        SessionBindingId,
        SessionBindingStatus,
        String,
        oneshot::Sender<Result<bool, StoreError>>,
    ),
    MarkDisconnected(AgentId, String, oneshot::Sender<Result<usize, StoreError>>),
    InsertPermission(PermissionDecision, oneshot::Sender<Result<(), StoreError>>),
    GetPermission(
        String,
        oneshot::Sender<Result<Option<PermissionDecision>, StoreError>>,
    ),
    Shutdown(Option<oneshot::Sender<()>>),
}

pub struct StorageWorker {
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl StorageWorker {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let path = PathBuf::from(path.as_ref());
        let (commands, receiver) = mpsc::channel(STORAGE_CAPACITY);
        let (started, ready) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let store = match SqliteStore::open(path) {
                Ok(store) => {
                    let _ = started.send(Ok(()));
                    store
                }
                Err(error) => {
                    let _ = started.send(Err(error));
                    return;
                }
            };
            run(store, receiver);
        });
        match ready.recv().map_err(|_| RuntimeError::ChannelClosed)? {
            Ok(()) => {}
            Err(error) => {
                thread
                    .join()
                    .map_err(|_| RuntimeError::StorageWorkerPanicked)?;
                return Err(error.into());
            }
        }
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    pub async fn insert_session_binding(
        &self,
        binding: SessionBinding,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| Command::InsertBinding(binding, reply))
            .await
    }

    pub async fn get_current_session_binding(
        &self,
        conversation_id: ConversationId,
        agent_id: AgentId,
    ) -> Result<Option<SessionBinding>, RuntimeError> {
        self.request(|reply| Command::GetCurrentBinding(conversation_id, agent_id, reply))
            .await
    }

    pub async fn list_current_session_bindings_for_agent(
        &self,
        agent_id: AgentId,
    ) -> Result<Vec<SessionBinding>, RuntimeError> {
        self.request(|reply| Command::ListCurrentBindings(agent_id, reply))
            .await
    }

    pub async fn update_session_binding_status(
        &self,
        id: SessionBindingId,
        status: SessionBindingStatus,
        last_used_at: String,
    ) -> Result<bool, RuntimeError> {
        self.request(|reply| Command::UpdateBindingStatus(id, status, last_used_at, reply))
            .await
    }

    pub async fn mark_current_bindings_disconnected(
        &self,
        agent_id: AgentId,
        last_used_at: String,
    ) -> Result<usize, RuntimeError> {
        self.request(|reply| Command::MarkDisconnected(agent_id, last_used_at, reply))
            .await
    }

    pub async fn insert_permission_decision(
        &self,
        decision: PermissionDecision,
    ) -> Result<(), RuntimeError> {
        self.request(|reply| Command::InsertPermission(decision, reply))
            .await
    }

    pub async fn get_permission_decision(
        &self,
        id: String,
    ) -> Result<Option<PermissionDecision>, RuntimeError> {
        self.request(|reply| Command::GetPermission(id, reply))
            .await
    }

    pub async fn shutdown(&mut self) -> Result<(), RuntimeError> {
        let (done, stopped) = oneshot::channel();
        self.commands
            .send(Command::Shutdown(Some(done)))
            .await
            .map_err(|_| RuntimeError::ChannelClosed)?;
        stopped.await.map_err(|_| RuntimeError::ChannelClosed)?;
        self.join()
    }

    async fn request<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<R, StoreError>>) -> Command,
    ) -> Result<R, RuntimeError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| RuntimeError::ChannelClosed)?;
        Ok(response.await.map_err(|_| RuntimeError::ChannelClosed)??)
    }

    fn join(&mut self) -> Result<(), RuntimeError> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| RuntimeError::StorageWorkerPanicked)?;
        }
        Ok(())
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let (closed, receiver) = mpsc::channel(1);
            drop(receiver);
            let commands = std::mem::replace(&mut self.commands, closed);
            let _ = commands.try_send(Command::Shutdown(None));
            drop(commands);
            let _ = self.join();
        }
    }
}

fn run(store: SqliteStore, mut commands: mpsc::Receiver<Command>) {
    while let Some(command) = commands.blocking_recv() {
        match command {
            Command::InsertBinding(binding, reply) => {
                let _ = reply.send(store.insert_session_binding(&binding));
            }
            Command::GetCurrentBinding(conversation_id, agent_id, reply) => {
                let _ = reply.send(store.get_current_session_binding(conversation_id, agent_id));
            }
            Command::ListCurrentBindings(agent_id, reply) => {
                let _ = reply.send(store.list_current_session_bindings_for_agent(agent_id));
            }
            Command::UpdateBindingStatus(id, status, last_used_at, reply) => {
                let _ = reply.send(store.update_session_binding_status(id, status, &last_used_at));
            }
            Command::MarkDisconnected(agent_id, last_used_at, reply) => {
                let _ =
                    reply.send(store.mark_current_bindings_disconnected(agent_id, &last_used_at));
            }
            Command::InsertPermission(decision, reply) => {
                let _ = reply.send(store.insert_permission_decision(&decision));
            }
            Command::GetPermission(id, reply) => {
                let _ = reply.send(store.get_permission_decision(&id));
            }
            Command::Shutdown(done) => {
                if let Some(done) = done {
                    let _ = done.send(());
                }
                return;
            }
        }
    }
}
