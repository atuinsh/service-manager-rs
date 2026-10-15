#[cfg(backend_launchd)]
use super::LaunchdServiceManager;
#[cfg(backend_openrc)]
use super::OpenRcServiceManager;
#[cfg(backend_rcd)]
use super::RcdServiceManager;
#[cfg(backend_sc)]
use super::ScServiceManager;
#[cfg(backend_systemd)]
use super::SystemdServiceManager;
#[cfg(backend_winsw)]
use super::WinSwServiceManager;
use super::{
    ServiceInstallCtx, ServiceLevel, ServiceManager, ServiceManagerKind, ServiceStartCtx,
    ServiceStopCtx, ServiceUninstallCtx,
};
use std::io;

/// Represents an implementation of a known [`ServiceManager`]
///
/// Holds a variant for each service manager this build compiles. Another crate in the build can
/// add more through features, so it is `#[non_exhaustive]`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TypedServiceManager {
    #[cfg(backend_launchd)]
    Launchd(LaunchdServiceManager),
    #[cfg(backend_openrc)]
    OpenRc(OpenRcServiceManager),
    #[cfg(backend_rcd)]
    Rcd(RcdServiceManager),
    #[cfg(backend_sc)]
    Sc(ScServiceManager),
    #[cfg(backend_systemd)]
    Systemd(SystemdServiceManager),
    #[cfg(backend_winsw)]
    WinSw(WinSwServiceManager),
}

macro_rules! using {
    ($self:ident, $this:ident -> $expr:expr) => {{
        match $self {
            #[cfg(backend_launchd)]
            TypedServiceManager::Launchd($this) => $expr,
            #[cfg(backend_openrc)]
            TypedServiceManager::OpenRc($this) => $expr,
            #[cfg(backend_rcd)]
            TypedServiceManager::Rcd($this) => $expr,
            #[cfg(backend_sc)]
            TypedServiceManager::Sc($this) => $expr,
            #[cfg(backend_systemd)]
            TypedServiceManager::Systemd($this) => $expr,
            #[cfg(backend_winsw)]
            TypedServiceManager::WinSw($this) => $expr,
        }
    }};
}

/// Whether `$self` is the `$variant` backend, false when `$backend` is not built.
macro_rules! is_variant {
    ($self:ident, $backend:meta, $variant:ident) => {
        match $self {
            #[cfg($backend)]
            TypedServiceManager::$variant(_) => true,
            // Unreachable when this is the only backend built, as launchd is on macOS.
            #[allow(unreachable_patterns)]
            _ => false,
        }
    };
}

impl ServiceManager for TypedServiceManager {
    fn available(&self) -> io::Result<bool> {
        using!(self, x -> x.available())
    }

    fn install(&self, ctx: ServiceInstallCtx) -> io::Result<()> {
        using!(self, x -> x.install(ctx))
    }

    fn uninstall(&self, ctx: ServiceUninstallCtx) -> io::Result<()> {
        using!(self, x -> x.uninstall(ctx))
    }

    fn start(&self, ctx: ServiceStartCtx) -> io::Result<()> {
        using!(self, x -> x.start(ctx))
    }

    fn stop(&self, ctx: ServiceStopCtx) -> io::Result<()> {
        using!(self, x -> x.stop(ctx))
    }

    fn level(&self) -> ServiceLevel {
        using!(self, x -> x.level())
    }

    fn set_level(&mut self, level: ServiceLevel) -> io::Result<()> {
        using!(self, x -> x.set_level(level))
    }

    fn status(&self, ctx: crate::ServiceStatusCtx) -> io::Result<crate::ServiceStatus> {
        using!(self, x -> x.status(ctx))
    }
}

impl TypedServiceManager {
    /// Creates a new service using the specified type, falling back to selecting
    /// based on native service manager for the current operating system if no type provided
    pub fn target_or_native(kind: impl Into<Option<ServiceManagerKind>>) -> io::Result<Self> {
        match kind.into() {
            Some(kind) => Ok(Self::target(kind)),
            None => Self::native(),
        }
    }

    /// Creates a new service manager targeting the specific service manager kind using the
    /// default service manager instance
    pub fn target(kind: ServiceManagerKind) -> Self {
        match kind {
            #[cfg(backend_launchd)]
            ServiceManagerKind::Launchd => Self::Launchd(LaunchdServiceManager::default()),
            #[cfg(backend_openrc)]
            ServiceManagerKind::OpenRc => Self::OpenRc(OpenRcServiceManager::default()),
            #[cfg(backend_rcd)]
            ServiceManagerKind::Rcd => Self::Rcd(RcdServiceManager::default()),
            #[cfg(backend_sc)]
            ServiceManagerKind::Sc => Self::Sc(ScServiceManager::default()),
            #[cfg(backend_systemd)]
            ServiceManagerKind::Systemd => Self::Systemd(SystemdServiceManager::default()),
            #[cfg(backend_winsw)]
            ServiceManagerKind::WinSw => Self::WinSw(WinSwServiceManager::default()),
        }
    }

    /// Attempts to select the native service manager for the current operating system
    ///
    /// * For MacOS, this will use [`LaunchdServiceManager`]
    /// * For Windows, this will use [`ScServiceManager`]
    /// * For BSD variants, this will use [`RcdServiceManager`]
    /// * For Linux variants, this will use either [`SystemdServiceManager`] or [`OpenRcServiceManager`]
    pub fn native() -> io::Result<Self> {
        Ok(Self::target(ServiceManagerKind::native()?))
    }

    /// Consumes underlying [`ServiceManager`] and moves it onto the heap
    pub fn into_box(self) -> Box<dyn ServiceManager> {
        using!(self, x -> Box::new(x))
    }

    /// Returns true if [`ServiceManager`] instance is for `launchd`
    pub fn is_launchd(&self) -> bool {
        is_variant!(self, backend_launchd, Launchd)
    }

    /// Returns true if [`ServiceManager`] instance is for `OpenRC`
    pub fn is_openrc(&self) -> bool {
        is_variant!(self, backend_openrc, OpenRc)
    }

    /// Returns true if [`ServiceManager`] instance is for `rc.d`
    pub fn is_rc_d(&self) -> bool {
        is_variant!(self, backend_rcd, Rcd)
    }

    /// Returns true if [`ServiceManager`] instance is for `sc`
    pub fn is_sc(&self) -> bool {
        is_variant!(self, backend_sc, Sc)
    }

    /// Returns true if [`ServiceManager`] instance is for `systemd`
    pub fn is_systemd(&self) -> bool {
        is_variant!(self, backend_systemd, Systemd)
    }

    /// Returns true if [`ServiceManager`] instance is for `winsw`
    pub fn is_winsw(&self) -> bool {
        is_variant!(self, backend_winsw, WinSw)
    }
}

#[cfg(backend_launchd)]
impl From<super::LaunchdServiceManager> for TypedServiceManager {
    fn from(manager: super::LaunchdServiceManager) -> Self {
        Self::Launchd(manager)
    }
}

#[cfg(backend_openrc)]
impl From<super::OpenRcServiceManager> for TypedServiceManager {
    fn from(manager: super::OpenRcServiceManager) -> Self {
        Self::OpenRc(manager)
    }
}

#[cfg(backend_rcd)]
impl From<super::RcdServiceManager> for TypedServiceManager {
    fn from(manager: super::RcdServiceManager) -> Self {
        Self::Rcd(manager)
    }
}

#[cfg(backend_sc)]
impl From<super::ScServiceManager> for TypedServiceManager {
    fn from(manager: super::ScServiceManager) -> Self {
        Self::Sc(manager)
    }
}

#[cfg(backend_systemd)]
impl From<super::SystemdServiceManager> for TypedServiceManager {
    fn from(manager: super::SystemdServiceManager) -> Self {
        Self::Systemd(manager)
    }
}

#[cfg(backend_winsw)]
impl From<super::WinSwServiceManager> for TypedServiceManager {
    fn from(manager: super::WinSwServiceManager) -> Self {
        Self::WinSw(manager)
    }
}
