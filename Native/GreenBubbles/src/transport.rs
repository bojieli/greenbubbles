use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::archive::ensure_private_directory;
use crate::connector::{
    ConnectorRequest, ConnectorRequestHandler, ConnectorResponse, CONNECTOR_API_VERSION,
};
use crate::RestoreError;

const MAX_CONNECTOR_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_CONNECTOR_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const CONNECTOR_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTOR_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECTOR_CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTOR_CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(300);

pub fn serve_unix(
    service: &impl ConnectorRequestHandler,
    socket_path: &Path,
) -> Result<(), RestoreError> {
    let (listener, _lease) = bind_private_socket(socket_path)?;
    for connection in listener.incoming() {
        match connection {
            Ok(mut connection) => {
                // A malformed, abandoned, or reset client must not terminate the long-running
                // connector. The request is unauthenticated until its complete envelope has been
                // read, so connection-level failures are deliberately isolated here.
                let _ = handle_connection(service, &mut connection);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn serve_unix_once(
    service: &impl ConnectorRequestHandler,
    socket_path: &Path,
) -> Result<(), RestoreError> {
    let (listener, _lease) = bind_private_socket(socket_path)?;
    let (mut connection, _) = listener.accept()?;
    handle_connection(service, &mut connection)
}

pub fn send_unix_request(
    socket_path: &Path,
    request: &ConnectorRequest,
) -> Result<ConnectorResponse, RestoreError> {
    let bytes = serde_json::to_vec(request)?;
    if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
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
    stream.set_write_timeout(Some(CONNECTOR_CLIENT_WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(CONNECTOR_CLIENT_READ_TIMEOUT))?;
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
    let response: ConnectorResponse = serde_json::from_slice(&response)?;
    if response.api_version != CONNECTOR_API_VERSION || response.request_id != request.request_id {
        return Err(RestoreError::Integrity(
            "connector response envelope does not match its request".to_string(),
        ));
    }
    Ok(response)
}

pub fn load_connector_request(path: &Path) -> Result<ConnectorRequest, RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    let identity = PrivateRequestIdentity::from_metadata(&before)?;
    if identity.byte_count > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
    let mut bytes = Vec::with_capacity(identity.byte_count as usize);
    (&mut file)
        .take(MAX_CONNECTOR_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
    let after = file.metadata()?;
    if PrivateRequestIdentity::from_metadata(&after)? != identity
        || bytes.len() as u64 != identity.byte_count
    {
        return Err(RestoreError::Integrity(
            "connector request changed while it was being read".to_string(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrivateRequestIdentity {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl PrivateRequestIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Result<Self, RestoreError> {
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(RestoreError::Integrity(
                "connector request must be a current-user, owner-only regular file".to_string(),
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            byte_count: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

fn bind_private_socket(path: &Path) -> Result<(UnixListener, SocketLease), RestoreError> {
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
    let lease = SocketLease::capture(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    validate_socket(path)?;
    Ok((listener, lease))
}

fn validate_socket(path: &Path) -> Result<(), RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(RestoreError::Integrity(
            "connector socket must be a current-user, owner-only Unix socket".to_string(),
        ));
    }
    Ok(())
}

fn handle_connection(
    service: &impl ConnectorRequestHandler,
    connection: &mut UnixStream,
) -> Result<(), RestoreError> {
    connection.set_read_timeout(Some(CONNECTOR_REQUEST_READ_TIMEOUT))?;
    connection.set_write_timeout(Some(CONNECTOR_RESPONSE_WRITE_TIMEOUT))?;
    let mut bytes = Vec::new();
    connection
        .take(MAX_CONNECTOR_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let response = if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        invalid_transport_response("transport", "connector request exceeds its byte limit")
    } else {
        match serde_json::from_slice::<ConnectorRequest>(&bytes) {
            Ok(request) => service.handle_connector_request(request),
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

struct SocketLease {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketLease {
    fn capture(path: &Path) -> Result<Self, RestoreError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket() {
            return Err(RestoreError::Integrity(
                "connector socket lease target is not a Unix socket".to_string(),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for SocketLease {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::os::unix::net::UnixStream;

    use tempfile::tempdir;

    use super::*;
    use crate::connector::{
        ConnectorDestination, ConnectorOperation, ConnectorRequest, CONNECTOR_API_VERSION,
    };

    #[test]
    fn socket_lease_removes_only_the_socket_it_created() {
        let fixture = tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = fixture.path().join("connector.sock");
        let (listener, lease) = bind_private_socket(&socket).unwrap();
        drop(listener);
        fs::remove_file(&socket).unwrap();
        fs::write(&socket, b"replacement").unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();

        drop(lease);

        assert_eq!(fs::read(&socket).unwrap(), b"replacement");
    }

    #[test]
    fn connector_request_is_read_from_one_private_descriptor() {
        let fixture = tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = fixture.path().join("request.json");
        let request = ConnectorRequest {
            api_version: CONNECTOR_API_VERSION.to_string(),
            request_id: "descriptor-request".to_string(),
            requester_id: "transport-test".to_string(),
            destination: ConnectorDestination::Local,
            operation: ConnectorOperation::Capabilities,
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&serde_json::to_vec(&request).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        drop(file);

        let loaded = load_connector_request(&path).unwrap();
        assert_eq!(loaded.request_id, request.request_id);
        assert_eq!(loaded.requester_id, request.requester_id);

        let hard_link = fixture.path().join("request-link.json");
        fs::hard_link(&path, &hard_link).unwrap();
        assert!(load_connector_request(&path).is_err());
    }

    #[test]
    fn client_rejects_a_response_for_a_different_request() {
        let fixture = tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = fixture.path().join("connector.sock");
        let (listener, lease) = bind_private_socket(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            connection.read_to_end(&mut request).unwrap();
            let response = invalid_transport_response("different-request", "synthetic");
            connection
                .write_all(&serde_json::to_vec(&response).unwrap())
                .unwrap();
            drop(lease);
        });
        let request = ConnectorRequest {
            api_version: CONNECTOR_API_VERSION.to_string(),
            request_id: "expected-request".to_string(),
            requester_id: "transport-test".to_string(),
            destination: ConnectorDestination::Local,
            operation: ConnectorOperation::Capabilities,
        };

        let error = send_unix_request(&socket, &request).unwrap_err();
        assert!(error
            .to_string()
            .contains("response envelope does not match"));
        server.join().unwrap();
    }

    #[test]
    fn a_server_read_deadline_bounds_an_incomplete_request() {
        let (mut reader, _writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let started = std::time::Instant::now();
        let error = reader.read(&mut [0_u8]).unwrap_err();

        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
