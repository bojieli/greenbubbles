use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::connector::{
    ConnectorRequest, ConnectorResponse, ConnectorService, CONNECTOR_API_VERSION,
};
use crate::RestoreError;

const MAX_CONNECTOR_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_CONNECTOR_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

pub fn serve_unix(service: &ConnectorService<'_>, socket_path: &Path) -> Result<(), RestoreError> {
    let listener = bind_private_socket(socket_path)?;
    let _lease = SocketLease(socket_path.to_path_buf());
    for connection in listener.incoming() {
        let mut connection = connection?;
        handle_connection(service, &mut connection)?;
    }
    Ok(())
}

pub fn serve_unix_once(
    service: &ConnectorService<'_>,
    socket_path: &Path,
) -> Result<(), RestoreError> {
    let listener = bind_private_socket(socket_path)?;
    let _lease = SocketLease(socket_path.to_path_buf());
    let (mut connection, _) = listener.accept()?;
    handle_connection(service, &mut connection)
}

pub fn send_unix_request(
    socket_path: &Path,
    request: &ConnectorRequest,
) -> Result<ConnectorResponse, RestoreError> {
    let mut validation = validate_socket(socket_path);
    for _ in 0..1_000 {
        if validation.is_ok() {
            break;
        }
        std::thread::yield_now();
        validation = validate_socket(socket_path);
    }
    validation?;
    let mut stream = UnixStream::connect(socket_path)?;
    let bytes = serde_json::to_vec(request)?;
    if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
    stream.write_all(&bytes)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    stream
        .take(MAX_CONNECTOR_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)?;
    if response.len() as u64 > MAX_CONNECTOR_RESPONSE_BYTES {
        return Err(RestoreError::Integrity(
            "connector response exceeds its byte limit".to_string(),
        ));
    }
    Ok(serde_json::from_slice(&response)?)
}

pub fn load_connector_request(path: &Path) -> Result<ConnectorRequest, RestoreError> {
    ensure_private_regular_file(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn bind_private_socket(path: &Path) -> Result<UnixListener, RestoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("socket path has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    if path.try_exists()? {
        return Err(RestoreError::Integrity(
            "connector socket path already exists".to_string(),
        ));
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    validate_socket(path)?;
    Ok(listener)
}

fn validate_socket(path: &Path) -> Result<(), RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(RestoreError::Integrity(
            "connector socket must be an owner-only Unix socket".to_string(),
        ));
    }
    Ok(())
}

fn handle_connection(
    service: &ConnectorService<'_>,
    connection: &mut UnixStream,
) -> Result<(), RestoreError> {
    let mut bytes = Vec::new();
    connection
        .take(MAX_CONNECTOR_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let response = if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        invalid_transport_response("transport", "connector request exceeds its byte limit")
    } else {
        match serde_json::from_slice::<ConnectorRequest>(&bytes) {
            Ok(request) => service.handle(request),
            Err(error) => invalid_transport_response("transport", &error.to_string()),
        }
    };
    let encoded = serde_json::to_vec(&response)?;
    if encoded.len() as u64 > MAX_CONNECTOR_RESPONSE_BYTES {
        return Err(RestoreError::Integrity(
            "connector response exceeds its byte limit".to_string(),
        ));
    }
    connection.write_all(&encoded)?;
    connection.flush()?;
    Ok(())
}

fn invalid_transport_response(request_id: &str, message: &str) -> ConnectorResponse {
    ConnectorResponse {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: request_id.to_string(),
        ok: false,
        result: None,
        error: Some(crate::connector::ConnectorErrorBody {
            code: crate::connector::ConnectorErrorCode::InvalidRequest,
            message: message.to_string(),
            retryable: false,
        }),
    }
}

struct SocketLease(PathBuf);

impl Drop for SocketLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

trait FileTypeSocket {
    fn is_socket(&self) -> bool;
}

impl FileTypeSocket for fs::FileType {
    fn is_socket(&self) -> bool {
        std::os::unix::fs::FileTypeExt::is_socket(self)
    }
}
