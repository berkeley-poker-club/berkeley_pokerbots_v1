use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use std::process::Stdio;
use std::time::Duration;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use poker_utils::{Action, PlayerId};
use crate::player_interface::{Player, PublicEvent, DecisionContext, LegalActions, PlayerError};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionRequest {
    pub context: DecisionContext,
    pub legal: LegalActions,
}

#[derive(Debug)]
pub enum BotError {
    SpawnFailed(std::io::Error),
    CommunicationFailed,
    ProcessDied,
}

impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            BotError::SpawnFailed(e) => write!(f, "BotError::SpawnFailed: {}", e),
            BotError::CommunicationFailed => write!(f, "BotError::CommunicationFailed"),
            BotError::ProcessDied => write!(f, "BotError::ProcessDied"),
        }
    }
}

impl std::error::Error for BotError {}

pub struct ProcessBot {
    player_id: PlayerId,
    // submission_id: SubmissionId,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    process_handle: Arc<Mutex<tokio::process::Child>>,
}

#[async_trait]
impl Player for ProcessBot {
    async fn notify_event(&self, event: &PublicEvent) -> Result<(), PlayerError> {
        let message = serde_json::to_string(event)
            .map_err(|_| PlayerError::CommunicationFailed)?;

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(message.as_bytes()).await
            .map_err(|_| PlayerError::CommunicationFailed)?;
        stdin.write_all(b"\n").await
            .map_err(|_| PlayerError::CommunicationFailed)?;
        stdin.flush().await
            .map_err(|_| PlayerError::CommunicationFailed)?;

        Ok(())
    }

    async fn request_action(
        &self,
        context: &DecisionContext,
        legal: &LegalActions,
        timeout_ms: u64,
    ) -> Result<Action, PlayerError> {
        let request = ActionRequest {
            context: context.clone(),
            legal: legal.clone()
        };
        let message = serde_json::to_string(&request)
            .map_err(|_| PlayerError::CommunicationFailed)?;

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(message.as_bytes()).await
                .map_err(|_| PlayerError::CommunicationFailed)?;
            stdin.write_all(b"\n").await
                .map_err(|_| PlayerError::CommunicationFailed)?;
            stdin.flush().await
                .map_err(|_| PlayerError::CommunicationFailed)?;
        }

        let response = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            async {
                let mut stdout = self.stdout.lock().await;
                let mut line = String::new();
                stdout.read_line(&mut line).await?;
                Ok::<String, std::io::Error>(line.trim().to_string())
            }
        ).await
        .map_err(|_| PlayerError::Timeout)?
        .map_err(|_| PlayerError::CommunicationFailed)?;

        serde_json::from_str(&response)
            .map_err(|_| PlayerError::InvalidResponse)
    }

    fn player_id(&self) -> PlayerId {
        self.player_id
    }
}

impl ProcessBot {
    pub async fn spawn(player_id: PlayerId, executable: &str, args: &[&str]) -> Result<Self, BotError> {
        let mut process = Command::new(executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(BotError::SpawnFailed)?;

        let stdin = process.stdin.take().ok_or(BotError::SpawnFailed(
            std::io::Error::new(std::io::ErrorKind::Other, "stdin failure")
        ))?;

        let stdout = process.stdout.take().ok_or(BotError::SpawnFailed(
            std::io::Error::new(std::io::ErrorKind::Other, "stdout failure")
        ))?;

        let stdout = BufReader::new(stdout);

        Ok(ProcessBot {
            player_id,
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(stdout)),
            process_handle: Arc::new(Mutex::new(process)),
        })
    }

    pub async fn is_alive(&self) -> bool {
        let mut process = self.process_handle.lock().await;
        match process.try_wait() {
            Ok(None) => true,       // process running
            Ok(Some(_)) => false,   // process exited
            Err(_) => false,
        }
    }

    pub async fn kill(&self) -> Result<(), BotError> {
        let mut process = self.process_handle.lock().await;
        process.kill().await.map_err(|_| BotError::ProcessDied)
    }
}