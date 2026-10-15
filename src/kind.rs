use cfg_if::cfg_if;
use std::io;

/// Represents the kind of service manager
///
/// Holds one variant per enabled backend feature, so it is `#[non_exhaustive]`: another crate in
/// the build can enable more of them.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(::clap::ValueEnum))]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
#[cfg_attr(feature = "clap", clap(rename_all = "lowercase"))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[non_exhaustive]
pub enum ServiceManagerKind {
    /// Use launchd to manage the service
    #[cfg(feature = "launchd")]
    Launchd,

    /// Use OpenRC to manage the service
    #[cfg(feature = "openrc")]
    OpenRc,

    /// Use rc.d to manage the service
    #[cfg(feature = "rcd")]
    Rcd,

    /// Use Windows service controller to manage the service
    #[cfg(feature = "sc")]
    Sc,

    /// Use systemd to manage the service
    #[cfg(feature = "systemd")]
    Systemd,

    /// Use WinSW to manage the service
    #[cfg(feature = "winsw")]
    WinSw,
}

impl ServiceManagerKind {
    /// Looks up the kind of service management platform native to the operating system
    ///
    /// Fails with `Unsupported` when the features for every service manager of the platform are
    /// disabled.
    pub fn native() -> io::Result<ServiceManagerKind> {
        cfg_if! {
            if #[cfg(all(target_os = "macos", feature = "launchd"))] {
                Ok(ServiceManagerKind::Launchd)
            } else if #[cfg(target_os = "macos")] {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "service-manager was built without the `launchd` feature",
                ))
            } else if #[cfg(target_os = "windows")] {
                // Prefer WinSW over sc.exe, because if it's present, it's likely been explicitly
                // installed as an alternative to sc.exe.
                #[cfg(feature = "winsw")]
                if is_available(ServiceManagerKind::WinSw) {
                    return Ok(ServiceManagerKind::WinSw);
                }

                cfg_if! {
                    if #[cfg(feature = "sc")] {
                        Ok(ServiceManagerKind::Sc)
                    } else if #[cfg(feature = "winsw")] {
                        Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "WinSW is not available and service-manager was built without the \
                             `sc` feature",
                        ))
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "service-manager was built without the `sc` and `winsw` features",
                        ))
                    }
                }
            } else if #[cfg(all(
                any(
                    target_os = "freebsd",
                    target_os = "dragonfly",
                    target_os = "openbsd",
                    target_os = "netbsd"
                ),
                feature = "rcd"
            ))] {
                Ok(ServiceManagerKind::Rcd)
            } else if #[cfg(any(
                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "openbsd",
                target_os = "netbsd"
            ))] {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "service-manager was built without the `rcd` feature",
                ))
            } else if #[cfg(all(target_os = "linux", any(feature = "systemd", feature = "openrc")))] {
                #[cfg(feature = "systemd")]
                if is_available(ServiceManagerKind::Systemd) {
                    return Ok(ServiceManagerKind::Systemd);
                }

                #[cfg(feature = "openrc")]
                if is_available(ServiceManagerKind::OpenRc) {
                    return Ok(ServiceManagerKind::OpenRc);
                }

                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Only systemd and openrc are supported on Linux",
                ))
            } else if #[cfg(target_os = "linux")] {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "service-manager was built without the `systemd` and `openrc` features",
                ))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Service manager are not supported on current Operating System!",
                ))
            }
        }
    }
}

/// Whether the default manager of `kind` reports itself available, treating a failed probe as no.
#[cfg(any(
    all(target_os = "windows", feature = "winsw"),
    all(target_os = "linux", any(feature = "systemd", feature = "openrc"))
))]
fn is_available(kind: ServiceManagerKind) -> bool {
    use super::{ServiceManager, TypedServiceManager};

    matches!(TypedServiceManager::target(kind).available(), Ok(true))
}
