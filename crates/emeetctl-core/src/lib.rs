//! Núcleo do emeetctl: descoberta do dispositivo, protocolo HID da luz e controles V4L2.
//!
//! O protocolo da luz foi decifrado por captura USB passiva do EMEET Studio 2.0.4
//! (firmware 1.0.19). Só os opcodes observados na captura são enviados.

pub mod device;
pub mod light;
pub mod usage;
pub mod video;

pub use device::{find, DeviceInfo, DevicePaths};
pub use light::{Light, LightStatus, Mode, Target};
pub use usage::{camera_users, describe as describe_users, CameraUser};
pub use video::{Control, ControlKind, Video};

#[derive(Debug)]
pub enum Error {
    NotFound(&'static str),
    Permission(String),
    Io(std::io::Error),
    Protocol(String),
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound(what) => write!(f, "EMEET S600L não encontrada ({what})"),
            Error::Permission(p) => write!(
                f,
                "sem permissão em {p}: instale udev/70-emeetctl.rules (ver README)"
            ),
            Error::Io(e) => write!(f, "erro de E/S: {e}"),
            Error::Protocol(m) => write!(f, "resposta inesperada da câmera: {m}"),
            Error::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn open_rw(path: &std::path::Path, nonblock: bool) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true);
    if nonblock {
        opts.custom_flags(libc::O_NONBLOCK);
    }
    opts.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => Error::Permission(path.display().to_string()),
        _ => Error::Io(e),
    })
}
