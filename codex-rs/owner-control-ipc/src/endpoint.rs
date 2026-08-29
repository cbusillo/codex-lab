use std::error::Error;
use std::fmt;
use std::path::Path;

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use crate::SessionError;

#[cfg(unix)]
use crate::DenyAllGestureSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    AcceptFailed,
    BindFailed,
    EndpointAlreadyExists,
    EndpointCompromised,
    IoConfigurationFailed,
    InvalidPath,
    InvalidPermissions,
    PeerRejected,
    UnsupportedPlatform,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::AcceptFailed => "owner-control IPC peer acceptance failed",
            Self::BindFailed => "owner-control IPC endpoint binding failed",
            Self::EndpointAlreadyExists => "owner-control IPC endpoint already exists",
            Self::EndpointCompromised => "owner-control IPC endpoint identity changed",
            Self::IoConfigurationFailed => "owner-control IPC stream configuration failed",
            Self::InvalidPath => "owner-control IPC endpoint path is invalid",
            Self::InvalidPermissions => "owner-control IPC endpoint permissions are invalid",
            Self::PeerRejected => "owner-control IPC peer identity was rejected",
            Self::UnsupportedPlatform => "owner-control IPC is unsupported on this platform",
        };
        formatter.write_str(message)
    }
}

impl Error for EndpointError {}

#[cfg(unix)]
pub struct OwnerControlEndpoint {
    listener: std::os::unix::net::UnixListener,
    parent_path: std::path::PathBuf,
    parent_device: u64,
    parent_inode: u64,
    socket_path: std::path::PathBuf,
    socket_device: u64,
    socket_inode: u64,
}

#[cfg(unix)]
impl OwnerControlEndpoint {
    pub fn bind(socket_path: impl AsRef<Path>) -> Result<Self, EndpointError> {
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        if !peer_credentials_supported() {
            return Err(EndpointError::UnsupportedPlatform);
        }
        let socket_path = socket_path.as_ref();
        if !socket_path.is_absolute() || socket_path.file_name().is_none() {
            return Err(EndpointError::InvalidPath);
        }
        let parent = socket_path.parent().ok_or(EndpointError::InvalidPath)?;
        let parent_metadata =
            std::fs::symlink_metadata(parent).map_err(|_| EndpointError::InvalidPath)?;
        let current_uid = unsafe { libc::getuid() };
        if !parent_metadata.file_type().is_dir()
            || parent_metadata.uid() != current_uid
            || parent_metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(EndpointError::InvalidPermissions);
        }
        match std::fs::symlink_metadata(socket_path) {
            Ok(_) => return Err(EndpointError::EndpointAlreadyExists),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(EndpointError::InvalidPath),
        }
        let listener = std::os::unix::net::UnixListener::bind(socket_path)
            .map_err(|_| EndpointError::BindFailed)?;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| EndpointError::InvalidPermissions)?;
        let socket_metadata =
            std::fs::symlink_metadata(socket_path).map_err(|_| EndpointError::BindFailed)?;
        let current_parent_metadata =
            std::fs::symlink_metadata(parent).map_err(|_| EndpointError::InvalidPermissions)?;
        if !current_parent_metadata.file_type().is_dir()
            || current_parent_metadata.uid() != current_uid
            || current_parent_metadata.permissions().mode() & 0o777 != 0o700
            || current_parent_metadata.dev() != parent_metadata.dev()
            || current_parent_metadata.ino() != parent_metadata.ino()
        {
            return Err(EndpointError::InvalidPermissions);
        }
        if !socket_metadata.file_type().is_socket()
            || socket_metadata.uid() != current_uid
            || socket_metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(EndpointError::InvalidPermissions);
        }
        Ok(Self {
            listener,
            parent_path: parent.to_path_buf(),
            parent_device: current_parent_metadata.dev(),
            parent_inode: current_parent_metadata.ino(),
            socket_path: socket_path.to_path_buf(),
            socket_device: socket_metadata.dev(),
            socket_inode: socket_metadata.ino(),
        })
    }

    pub fn serve_once(&self) -> Result<(), EndpointServeError> {
        self.validate_bound_endpoint()?;
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|_| EndpointError::AcceptFailed)?;
        self.validate_bound_endpoint()?;
        validate_unix_peer_owner(&stream)?;
        let mut stream = UnixDeadlineStream::new(stream, Duration::from_secs(5))?;
        crate::session::serve_stream(&mut stream, &DenyAllGestureSource).map_err(Into::into)
    }

    fn validate_bound_endpoint(&self) -> Result<(), EndpointError> {
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        let parent_metadata = std::fs::symlink_metadata(&self.parent_path)
            .map_err(|_| EndpointError::EndpointCompromised)?;
        let metadata = std::fs::symlink_metadata(&self.socket_path)
            .map_err(|_| EndpointError::EndpointCompromised)?;
        if !parent_metadata.file_type().is_dir()
            || parent_metadata.uid() != unsafe { libc::getuid() }
            || parent_metadata.permissions().mode() & 0o777 != 0o700
            || parent_metadata.dev() != self.parent_device
            || parent_metadata.ino() != self.parent_inode
            || !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::getuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.dev() != self.socket_device
            || metadata.ino() != self.socket_inode
        {
            return Err(EndpointError::EndpointCompromised);
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) struct UnixDeadlineStream {
    stream: std::os::unix::net::UnixStream,
    deadline: Instant,
}

#[cfg(unix)]
impl UnixDeadlineStream {
    pub(crate) fn new(
        stream: std::os::unix::net::UnixStream,
        timeout: Duration,
    ) -> Result<Self, EndpointError> {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|_| EndpointError::IoConfigurationFailed)?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|_| EndpointError::IoConfigurationFailed)?;
        Ok(Self {
            stream,
            deadline: Instant::now() + timeout,
        })
    }

    fn remaining(&self) -> std::io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "owner-control IPC session deadline elapsed",
                )
            })
    }
}

#[cfg(unix)]
impl Read for UnixDeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buffer)
    }
}

#[cfg(unix)]
impl Write for UnixDeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.flush()
    }
}

#[cfg(not(unix))]
pub struct OwnerControlEndpoint;

#[cfg(not(unix))]
impl OwnerControlEndpoint {
    pub fn bind(_socket_path: impl AsRef<Path>) -> Result<Self, EndpointError> {
        Err(EndpointError::UnsupportedPlatform)
    }

    pub fn serve_once(&self) -> Result<(), EndpointServeError> {
        Err(EndpointError::UnsupportedPlatform.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointServeError {
    Endpoint(EndpointError),
    Session(SessionError),
}

impl fmt::Display for EndpointServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(error) => error.fmt(formatter),
            Self::Session(error) => error.fmt(formatter),
        }
    }
}

impl Error for EndpointServeError {}

impl From<EndpointError> for EndpointServeError {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

impl From<SessionError> for EndpointServeError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
const fn peer_credentials_supported() -> bool {
    true
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))
))]
const fn peer_credentials_supported() -> bool {
    false
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn validate_unix_peer_owner(stream: &std::os::unix::net::UnixStream) -> Result<(), EndpointError> {
    use std::os::fd::AsRawFd;

    let mut credentials = unsafe { std::mem::zeroed::<libc::ucred>() };
    let mut credentials_length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
        .map_err(|_| EndpointError::PeerRejected)?;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut credentials_length,
        )
    };
    if result != 0 || credentials.uid != unsafe { libc::getuid() } {
        return Err(EndpointError::PeerRejected);
    }
    Ok(())
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn validate_unix_peer_owner(stream: &std::os::unix::net::UnixStream) -> Result<(), EndpointError> {
    use std::os::fd::AsRawFd;

    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) };
    if result != 0 || peer_uid != unsafe { libc::getuid() } {
        return Err(EndpointError::PeerRejected);
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))
))]
fn validate_unix_peer_owner(_stream: &std::os::unix::net::UnixStream) -> Result<(), EndpointError> {
    Err(EndpointError::UnsupportedPlatform)
}
