//! Unix socket IPC between `bakawm-ctl` and the running compositor.
//!
//! Protocol: length-prefixed JSON frames.
//! - Request:  `[u32 LE json_len][json_bytes]`
//! - Response: `[u32 LE json_len][json_bytes]` optionally followed by
//!   `[u32 LE bin_len][bin_bytes]` when the response carries binary data
//!   (e.g. PNG pixels for a screenshot).

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;

use calloop::generic::Generic;
use calloop::{Interest, Mode, PostAction};
use serde::{Deserialize, Serialize};

use crate::state::{AnvilState, Backend};

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcRequest {
    #[serde(rename = "list_windows")]
    ListWindows,
    #[serde(rename = "focus_window")]
    FocusWindow { id: u64 },
    #[serde(rename = "close_window")]
    CloseWindow { id: u64 },
    #[serde(rename = "move_window")]
    MoveWindow { id: u64, x: i32, y: i32 },
    #[serde(rename = "resize_window")]
    ResizeWindow { id: u64, w: i32, h: i32 },
    #[serde(rename = "screenshot")]
    Screenshot { output: Option<String> },
    #[serde(rename = "list_outputs")]
    ListOutputs,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IpcResponse {
    #[serde(rename = "windows")]
    Windows { windows: Vec<WindowInfo> },
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "screenshot")]
    Screenshot {
        png_length: u64,
        width: u32,
        height: u32,
    },
    #[serde(rename = "outputs")]
    Outputs { outputs: Vec<OutputInfo> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    /// (x, y, width, height) in logical coordinates, if available.
    pub geometry: Option<(i32, i32, i32, i32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

// ---------------------------------------------------------------------------
// Internal types (not serialized – sent through calloop channel)
// ---------------------------------------------------------------------------

/// Wraps a request together with the channel used to deliver the answer back
/// to the connection thread.
pub(crate) struct IpcMessage {
    pub request: IpcRequest,
    pub response_tx: mpsc::Sender<IpcResponseAndData>,
}

/// Response with optional trailing binary data (PNG bytes).
pub(crate) struct IpcResponseAndData {
    pub response: IpcResponse,
    pub binary: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Socket path
// ---------------------------------------------------------------------------

/// Resolve the IPC socket path.
///
/// Prefers `$XDG_RUNTIME_DIR/bakawm.sock`, falling back to a temp-dir path.
pub fn socket_path() -> PathBuf {
    if let Some(runtime) = dirs::runtime_dir() {
        runtime.join("bakawm.sock")
    } else {
        std::env::temp_dir().join("bakawm.sock")
    }
}

// ---------------------------------------------------------------------------
// Frame read / write helpers
// ---------------------------------------------------------------------------

fn write_request(stream: &mut UnixStream, request: &IpcRequest) -> std::io::Result<()> {
    let json = serde_json::to_vec(request)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = json.len() as u32;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&json)?;
    Ok(())
}

fn read_request(stream: &mut UnixStream) -> std::io::Result<IpcRequest> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request too large",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn write_response(
    stream: &mut UnixStream,
    response: &IpcResponse,
    binary: Option<&[u8]>,
) -> std::io::Result<()> {
    let json = serde_json::to_vec(response)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = json.len() as u32;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&json)?;

    if let Some(data) = binary {
        let bin_len = data.len() as u32;
        stream.write_all(&bin_len.to_le_bytes())?;
        stream.write_all(data)?;
    }
    Ok(())
}

fn read_response(stream: &mut UnixStream) -> std::io::Result<(IpcResponse, Option<Vec<u8>>)> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "response too large",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let response: IpcResponse = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let binary = if matches!(response, IpcResponse::Screenshot { .. }) {
        let mut bin_len_buf = [0u8; 4];
        stream.read_exact(&mut bin_len_buf)?;
        let bin_len = u32::from_le_bytes(bin_len_buf) as usize;
        let mut bin_buf = vec![0u8; bin_len];
        stream.read_exact(&mut bin_buf)?;
        Some(bin_buf)
    } else {
        None
    };

    Ok((response, binary))
}

// ---------------------------------------------------------------------------
// Server side (compositor)
// ---------------------------------------------------------------------------

/// Start the IPC server, registering listener + request channel on the
/// compositor's event loop.  Returns the socket path on success.
pub fn start_ipc_server<B: Backend + 'static>(
    state: &mut AnvilState<B>,
) -> Result<PathBuf, std::io::Error> {
    let path = socket_path();

    // Remove a stale socket file if present.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    let (to_state, from_ipc) = calloop::channel::channel::<IpcMessage>();

    // Channel source: delivers requests from connection threads to the state.
    state
        .handle
        .insert_source(from_ipc, |event, _, state| match event {
            calloop::channel::Event::Msg(msg) => {
                let IpcMessage {
                    request,
                    response_tx,
                } = msg;
                let (response, binary) = state.handle_ipc_request(request);
                let _ = response_tx.send(IpcResponseAndData { response, binary });
            }
            calloop::channel::Event::Closed => (),
        })
        .expect("Failed to insert IPC channel source");

    // Listener source: accept connections, hand each off to a worker thread.
    let to_state_clone = to_state.clone();
    state
        .handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, _| {
                while let Ok((stream, _)) = listener.accept() {
                    let to_state = to_state_clone.clone();
                    std::thread::spawn(move || handle_connection(stream, to_state));
                }
                Ok(PostAction::Continue)
            },
        )
        .expect("Failed to insert IPC listener source");

    Ok(path)
}

/// Per-connection handler running on a worker thread.
fn handle_connection(mut stream: UnixStream, to_state: calloop::channel::Sender<IpcMessage>) {
    let request = match read_request(&mut stream) {
        Ok(req) => req,
        Err(e) => {
            let _ = write_response(
                &mut stream,
                &IpcResponse::Error {
                    message: e.to_string(),
                },
                None,
            );
            return;
        }
    };

    let (response_tx, response_rx) = mpsc::channel::<IpcResponseAndData>();
    if to_state
        .send(IpcMessage {
            request,
            response_tx,
        })
        .is_err()
    {
        return;
    }

    let result = match response_rx.recv() {
        Ok(result) => result,
        Err(_) => {
            let _ = write_response(
                &mut stream,
                &IpcResponse::Error {
                    message: "compositor did not respond".to_owned(),
                },
                None,
            );
            return;
        }
    };

    let _ = write_response(&mut stream, &result.response, result.binary.as_deref());
}

// ---------------------------------------------------------------------------
// Client side (bakawm-ctl)
// ---------------------------------------------------------------------------

/// Send a single request to the running compositor and read the response.
pub fn send_request(
    request: &IpcRequest,
) -> Result<(IpcResponse, Option<Vec<u8>>), String> {
    let path = socket_path();
    let mut stream =
        UnixStream::connect(&path).map_err(|e| {
            format!(
                "Failed to connect to bakawm IPC socket ({}): {}",
                path.display(),
                e
            )
        })?;

    write_request(&mut stream, request).map_err(|e| format!("Failed to send request: {e}"))?;

    let (response, binary) =
        read_response(&mut stream).map_err(|e| format!("Failed to read response: {e}"))?;

    Ok((response, binary))
}

/// Convenience: check whether the IPC socket exists.
pub fn ipc_available() -> bool {
    socket_path().exists()
}
