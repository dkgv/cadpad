use crate::{Connection, Request, Response, WIRE_VERSION, protocol};
use cadpad_core::{
    Core, Error, Result, error_value,
    tools::{self, ToolContext},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{
        UnixListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, Semaphore, watch},
    task::{JoinHandle, JoinSet},
};

fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::domain(
        "APP_UNAVAILABLE",
        format!(
            "Start Cadpad, then reconnect the MCP server using its connection settings. {error}"
        ),
    )
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub actor: String,
    pub client_name: String,
    pub connected: bool,
    pub calls: u64,
}
struct Session {
    info: SessionInfo,
    disconnect: watch::Sender<bool>,
}
struct Shared {
    core: Core,
    sessions: Mutex<BTreeMap<String, Session>>,
}
struct Control {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: std::sync::Mutex<Option<JoinHandle<()>>>,
    connection_file: PathBuf,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
#[derive(Clone)]
pub struct AppServer {
    control: Arc<Control>,
}
impl AppServer {
    pub async fn start(core: Core, directory: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).await?;
        let socket_directory = tempfile::Builder::new().prefix("cp").tempdir()?;
        std::fs::set_permissions(
            socket_directory.path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let endpoint = socket_directory.path().join("s");
        if endpoint.as_os_str().as_encoded_bytes().len() > 100 {
            return Err(Error::domain(
                "APP_UNAVAILABLE",
                "Temporary directory is too long for a local Unix socket; set TMPDIR to a shorter private directory",
            ));
        }
        let listener = UnixListener::bind(&endpoint)?;
        tokio::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).await?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let connection = Connection {
            protocol_version: WIRE_VERSION,
            endpoint,
            token,
        };
        let connection_file = directory.join("connection.json");
        let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(&connection)?)?;
            file.sync_all()?;
        }
        tokio::fs::rename(temporary, &connection_file).await?;
        let shared = Arc::new(Shared {
            core,
            sessions: Mutex::new(BTreeMap::new()),
        });
        let (stop, mut stopped) = watch::channel(false);
        let task_shared = shared.clone();
        let cleanup_path = connection_file.clone();
        let task = tokio::spawn(async move {
            let _socket_directory = socket_directory;
            let connections = Arc::new(Semaphore::new(32));
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    _=stopped.changed()=>break,
                    Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
                    accepted=listener.accept()=>{
                        let Ok((stream,_))=accepted else{break;};
                        let Ok(permit)=connections.clone().try_acquire_owned() else{drop(stream);continue;};
                        let shared=task_shared.clone();let token=connection.token.clone();let stopped=stopped.clone();
                        tasks.spawn(async move{let _permit=permit;let _=serve_session(stream,shared,token,stopped).await;});
                    }
                }
            }
            let actors = {
                let mut sessions = task_shared.sessions.lock().await;
                sessions
                    .values_mut()
                    .map(|session| {
                        session.disconnect.send_replace(true);
                        session.info.actor.clone()
                    })
                    .collect::<Vec<_>>()
            };
            for actor in actors {
                task_shared.core.revoke_actor(&actor).await;
            }
            while tasks.join_next().await.is_some() {}
            // Never remove a newer server's credentials.
            if tokio::fs::read(&cleanup_path)
                .await
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Connection>(&bytes).ok())
                .is_some_and(|old| old.token == connection.token)
            {
                let _ = tokio::fs::remove_file(cleanup_path).await;
            }
        });
        Ok(Self {
            control: Arc::new(Control {
                shared,
                stop,
                task: std::sync::Mutex::new(Some(task)),
                connection_file,
            }),
        })
    }
    pub fn connection_file(&self) -> &Path {
        &self.control.connection_file
    }
    pub async fn sessions(&self) -> Vec<SessionInfo> {
        self.control
            .shared
            .sessions
            .lock()
            .await
            .values()
            .map(|session| session.info.clone())
            .collect()
    }
    pub async fn disconnect(&self, session_id: &str) -> Result<usize> {
        let actor = {
            let mut sessions = self.control.shared.sessions.lock().await;
            let session = sessions
                .get_mut(session_id)
                .ok_or_else(|| Error::domain("INVALID_REQUEST", "Unknown external session"))?;
            session.disconnect.send_replace(true);
            session.info.actor.clone()
        };
        Ok(self.control.shared.core.revoke_actor(&actor).await)
    }
    pub async fn shutdown(&self) {
        self.control.stop.send_replace(true);
        let task = self.control.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
async fn serve_session(
    stream: UnixStream,
    shared: Arc<Shared>,
    token: String,
    mut stopped: watch::Receiver<bool>,
) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    let request = tokio::time::timeout(
        Duration::from_secs(5),
        protocol::read::<_, Request>(&mut reader),
    )
    .await
    .map_err(|_| Error::domain("INVALID_AUTH", "Local session handshake timed out"))??;
    let Some(Request::Hello {
        protocol_version,
        token: presented,
        client_name,
    }) = request
    else {
        return Err(Error::domain(
            "INVALID_AUTH",
            "Authenticated hello required",
        ));
    };
    let valid =
        bool::from(Sha256::digest(token.as_bytes()).ct_eq(&Sha256::digest(presented.as_bytes())));
    if !valid
        || protocol_version != WIRE_VERSION
        || client_name.is_empty()
        || client_name.len() > 100
        || client_name.chars().any(char::is_control)
    {
        protocol::write(&mut writer,&Response{id:0,result:None,error:Some(json!({"code":"INVALID_AUTH","message":"Invalid local authentication or protocol version"}))}).await?;
        return Ok(());
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    let actor = format!("external:{client_name}:{session_id}");
    let (disconnect, mut disconnected) = watch::channel(false);
    let mut sessions = shared.sessions.lock().await;
    while sessions.len() >= 256 {
        let retired = sessions
            .iter()
            .find(|(_, session)| !session.info.connected)
            .map(|(id, _)| id.clone());
        if let Some(id) = retired {
            sessions.remove(&id);
        } else {
            break;
        }
    }
    sessions.insert(
        session_id.clone(),
        Session {
            info: SessionInfo {
                id: session_id.clone(),
                actor: actor.clone(),
                client_name,
                connected: true,
                calls: 0,
            },
            disconnect,
        },
    );
    drop(sessions);
    let writer = Arc::new(Mutex::new(writer));
    protocol::write(
        &mut *writer.lock().await,
        &Response {
            id: 0,
            result: Some(
                json!({"session_id":session_id,"actor":actor,"protocol_version":WIRE_VERSION}),
            ),
            error: None,
        },
    )
    .await?;
    let mut requests = JoinSet::new();
    let mut last_id = 0;
    loop {
        tokio::select! {
            _=stopped.changed()=>break,
            _=disconnected.changed()=>break,
            Some(_)=requests.join_next(),if !requests.is_empty()=>{},
            request=protocol::read::<_,Request>(&mut reader),if requests.len()<8=>{
                let Ok(Some(request))=request else{break;};
                let id=match &request{Request::Call{id,..}|Request::Definitions{id}|Request::Artifact{id,..}|Request::ArtifactChunk{id,..}=>*id,Request::Hello{..}=>break};
                if id<=last_id{break;}last_id=id;
                if let Some(session)=shared.sessions.lock().await.get_mut(&session_id){session.info.calls+=1;}
                let core=shared.core.clone();let actor=actor.clone();let writer=writer.clone();
                requests.spawn(async move{
                    let result=match request{
                        Request::Call{name,arguments,..}=>core.dispatch(&ToolContext{actor},&name,arguments).await,
                        Request::Artifact{workspace,revision,handle,..}=>{
                            match core.ensure_actor_active(&actor){Ok(())=>core.export_resource(&actor,&workspace,&revision,&handle,None).await,Err(error)=>Err(error)}
                        },
                        Request::ArtifactChunk{workspace,revision,handle,range,..}=>core.export_resource(&actor,&workspace,&revision,&handle,Some(range)).await,
                        Request::Definitions{..}=>{
                            let mut definitions=tools::definitions();
                            for tool in &mut definitions {
                                // Older bridges ignore mcp_output_schema and
                                // strip image/add URI content just like this
                                // bridge. Do not let them advertise the native
                                // schema for a differently shaped MCP result.
                                if tool.mcp_output_schema.is_some() {
                                    tool.output_schema=None;
                                }
                            }
                            Ok(json!(definitions))
                        },
                        Request::Hello{..}=>unreachable!(),
                    };
                    let response=match result{Ok(value)=>Response{id,result:Some(value),error:None},Err(error)=>Response{id,result:None,error:Some(error_value(&error))}};
                    let _=protocol::write(&mut *writer.lock().await,&response).await;
                });
            }
        }
    }
    shared.core.revoke_actor(&actor).await;
    // Let dispatcher transactions finish their atomic persistence boundary.
    // Actor revocation cancels native work and rejects any late build request.
    while requests.join_next().await.is_some() {}
    if let Some(session) = shared.sessions.lock().await.get_mut(&session_id) {
        session.info.connected = false;
    }
    let _ = writer.lock().await.shutdown().await;
    Ok(())
}
struct ClientIo {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    sequence: u64,
}
pub struct Client {
    io: Mutex<ClientIo>,
    pub session_id: String,
    pub actor: String,
}
impl Client {
    pub async fn connect(path: &Path, client_name: &str) -> Result<Self> {
        let metadata = tokio::fs::symlink_metadata(path)
            .await
            .map_err(unavailable)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > 65536
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(Error::domain(
                "INVALID_AUTH",
                "Connection credentials must be a private regular file (mode 0600)",
            ));
        }
        let connection: Connection =
            serde_json::from_slice(&tokio::fs::read(path).await.map_err(unavailable)?)?;
        if connection.protocol_version != WIRE_VERSION {
            return Err(unavailable("Incompatible bridge/app protocol version"));
        }
        let stream = UnixStream::connect(&connection.endpoint)
            .await
            .map_err(unavailable)?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        protocol::write(
            &mut writer,
            &Request::Hello {
                protocol_version: WIRE_VERSION,
                token: connection.token,
                client_name: client_name.into(),
            },
        )
        .await?;
        let response: Response =
            tokio::time::timeout(Duration::from_secs(10), protocol::read(&mut reader))
                .await
                .map_err(|_| unavailable("Authentication timed out"))??
                .ok_or_else(|| unavailable("App closed connection"))?;
        let result = decode(response, 0)?;
        Ok(Self {
            session_id: result["session_id"]
                .as_str()
                .ok_or_else(|| unavailable("Invalid session response"))?
                .into(),
            actor: result["actor"].as_str().unwrap().into(),
            io: Mutex::new(ClientIo {
                reader,
                writer,
                sequence: 0,
            }),
        })
    }
    pub async fn call(&self, name: &str, arguments: Value) -> Result<Value> {
        let mut io = self.io.lock().await;
        io.sequence += 1;
        let id = io.sequence;
        protocol::write(
            &mut io.writer,
            &Request::Call {
                id,
                name: name.into(),
                arguments,
            },
        )
        .await
        .map_err(unavailable)?;
        let response = protocol::read::<_, Response>(&mut io.reader)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("App disconnected this session"))?;
        decode(response, id)
    }
    pub async fn artifact(&self, workspace: &str, revision: &str, handle: &str) -> Result<Value> {
        let mut io = self.io.lock().await;
        io.sequence += 1;
        let id = io.sequence;
        protocol::write(
            &mut io.writer,
            &Request::Artifact {
                id,
                workspace: workspace.into(),
                revision: revision.into(),
                handle: handle.into(),
            },
        )
        .await
        .map_err(unavailable)?;
        let response = protocol::read::<_, Response>(&mut io.reader)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("App disconnected"))?;
        decode(response, id)
    }
    pub async fn artifact_chunk(
        &self,
        workspace: &str,
        revision: &str,
        handle: &str,
        range: cadpad_core::artifacts::Chunk,
    ) -> Result<Value> {
        let mut io = self.io.lock().await;
        io.sequence += 1;
        let id = io.sequence;
        protocol::write(
            &mut io.writer,
            &Request::ArtifactChunk {
                id,
                workspace: workspace.into(),
                revision: revision.into(),
                handle: handle.into(),
                range,
            },
        )
        .await
        .map_err(unavailable)?;
        let response = protocol::read::<_, Response>(&mut io.reader)
            .await
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("App disconnected"))?;
        decode(response, id)
    }
    pub async fn definitions(&self) -> Result<Value> {
        let mut io = self.io.lock().await;
        io.sequence += 1;
        let id = io.sequence;
        protocol::write(&mut io.writer, &Request::Definitions { id }).await?;
        decode(
            protocol::read::<_, Response>(&mut io.reader)
                .await?
                .ok_or_else(|| unavailable("App closed connection"))?,
            id,
        )
    }
}
fn decode(response: Response, id: u64) -> Result<Value> {
    if response.id != id {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "Mismatched local response ID",
        ));
    }
    if let Some(error) = response.error {
        return Err(Error::Domain {
            code: error["code"].as_str().unwrap_or("APP_UNAVAILABLE").into(),
            message: error["message"]
                .as_str()
                .unwrap_or("Local application request failed")
                .into(),
            details: error.get("details").cloned().unwrap_or(Value::Null),
        });
    }
    response
        .result
        .ok_or_else(|| unavailable("Empty application response"))
}
