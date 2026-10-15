use crate::utils::wrap_output;

use super::{
    utils, RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager,
    ServiceStartCtx, ServiceStopCtx, ServiceUninstallCtx,
};
use plist::{Dictionary, Value};
use rustix::{fs::Uid, process::getuid};
use std::{
    convert::Infallible,
    error::Error,
    ffi::OsStr,
    fmt::Display,
    fs::OpenOptions,
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    process::{self, Command, Output, Stdio},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

mod launchctl;

use launchctl::LaunchCtl;

/// The appropriate file permissions for a PLIST file.
const PLIST_FILE_PERMISSIONS: u32 = 0o644;

/// The longest file name APFS and HFS+ accept, in bytes.
const PATH_MAX: usize = 255;

/// The domain which a service can run.
///
/// `launchd` supports user, gui and system services. It supports more, but they appear to be
/// irrelevant.
///
/// System services are services which run on the system for all the time through which a system is
/// booted up.
///
/// Gui services (or gui agents) start when a user logs into the graphical session and last for as
/// long as the user is logged in.
///
/// User services are background services that run on a per-user basis. The lifetime appears
/// unclear, as the man page says:
///
/// > Targets the user domain for the given UID or a service within that domain. A user domain may
/// > exist independently of a logged-in user.
///
/// For more details on these, please see `man launchctl`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceDomain {
    /// `user/<uid>/` services.
    User(Uid),
    /// `gui/<uid>/` services.
    Gui(Uid),
    /// `system/` services.
    #[default]
    System,
}

impl ServiceDomain {
    /// Infer the service domain from the active execution context.
    ///
    /// Deprecated `launchctl` commands would infer the domain in which services would run. For
    /// example, `launchctl load` would request a service be loaded into an inferred domain. This
    /// function models this inference.
    #[must_use]
    pub fn infer() -> Self {
        let uid = rustix::process::getuid();

        if uid.is_root() {
            ServiceDomain::System
        } else {
            ServiceDomain::Gui(uid)
        }
    }

    /// Determine the appropriate path for creating service plists.
    #[must_use]
    pub fn plist_dir(self) -> PathBuf {
        match self {
            ServiceDomain::User(uid) => unimplemented!("TODO"),
            ServiceDomain::Gui(uid) => unimplemented!("TODO"),
            ServiceDomain::System => PathBuf::from("/Library/LaunchDaemons"),
        }
    }
}

impl From<ServiceDomain> for String {
    fn from(value: ServiceDomain) -> String {
        match value {
            ServiceDomain::User(uid) => format!("user/{}", uid.as_raw()),
            ServiceDomain::Gui(uid) => format!("gui/{}", uid.as_raw()),
            ServiceDomain::System => "system".into(),
        }
    }
}

/// Configuration settings tied to launchd services
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchdConfig {
    pub install: LaunchdInstallConfig,
}

/// Configuration settings tied to launchd services during installation
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchdInstallConfig {
    /// Launchd-specific keep-alive setting. If `Some`, this takes precedence over the generic
    /// `RestartPolicy` in `ServiceInstallCtx`. If `None`, the generic policy is used.
    pub keep_alive: Option<bool>,
}

impl Default for LaunchdInstallConfig {
    fn default() -> Self {
        Self { keep_alive: None }
    }
}

/// Implementation of [`ServiceManager`] for MacOS's [Launchd](https://en.wikipedia.org/wiki/Launchd)
///
/// System services live in the `system` domain. User services live in the calling user's
/// `gui/<uid>` domain, which exists only while that user is logged in graphically.
///
/// TODO(markovejnovic): Why gui only?
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchdServiceManager {
    /// The domain the service covers.
    pub domain: ServiceDomain,

    /// Configuration settings tied to launchd services
    pub config: LaunchdConfig,
}

impl LaunchdServiceManager {
    /// Creates a new manager instance working with system services
    pub fn system() -> Self {
        Self::default()
    }

    /// Creates a new manager instance working with user services
    pub fn user() -> Self {
        Self::default().into_user()
    }

    /// Change manager to work with system services
    pub fn into_system(self) -> Self {
        Self {
            config: self.config,
            domain: ServiceDomain::System,
        }
    }

    /// Change manager to work with user services
    pub fn into_user(self) -> Self {
        Self {
            config: self.config,
            domain: ServiceDomain::User(rustix::process::getuid()),
        }
    }

    /// Update manager to use the specified config
    pub fn with_config(self, config: LaunchdConfig) -> Self {
        Self { config, ..self }
    }

    fn plist_dir(&self) -> PathBuf {
        self.domain.plist_dir()
    }
}

impl ServiceManager for LaunchdServiceManager {
    fn available(&self) -> io::Result<bool> {
        Ok(LaunchCtl::available())
    }

    fn install(&self, ctx: ServiceInstallCtx) -> io::Result<()> {
        std::fs::create_dir_all(self.plist_dir())?;

        let qualified_name = ctx.label.to_qualified_name();
        let plist = match ctx.contents {
            Some(contents) => contents,
            _ => make_plist(
                &self.config.install,
                &qualified_name,
                ctx.cmd_iter(),
                ctx.username.clone(),
                ctx.working_directory.clone(),
                ctx.environment.clone(),
                ctx.autostart,
                ctx.restart_policy,
            ),
        };

        service.bootout()?;
        utils::write_file(
            service.plist_path.as_path(),
            plist.as_bytes(),
            PLIST_FILE_PERMISSIONS,
        )?;

        // bootstrap refuses a plist that sets Disabled, which make_plist does for KeepAlive
        // services so that installing one does not start it. start() lifts it.
        if sets_disabled(plist.as_bytes()) {
            return Ok(());
        }

        // As with `launchctl load`, RunAtLoad (autostart) runs the job now. A plist launchd
        // refuses is removed again so that a failed install() leaves nothing installed; the
        // bootstrap failure is the error worth reporting.
        service.bootstrap().map_err(|err| {
            let _ = std::fs::remove_file(&service.plist_path);
            err
        })
    }

    fn uninstall(&self, ctx: ServiceUninstallCtx) -> io::Result<()> {
        let service = self.service(&ctx.label)?;
        service.bootout()?;
        std::fs::remove_file(&service.plist_path).or_else(|err| match err.kind() {
            io::ErrorKind::NotFound => Ok(()),
            _ => Err(err),
        })
    }

    /// Starts a service, loading it into its launchd domain first if it is not loaded.
    ///
    /// Blocks while launchd tears down a job that is being stopped, and while it throttles a job
    /// that exited less than its ThrottleInterval (`10s` by default) ago.
    fn start(&self, ctx: ServiceStartCtx) -> io::Result<()> {
        let service = self.service(&ctx.label)?;
        if !service.plist_path.try_exists()? {
            return Err(not_installed(&ctx.label));
        }

        // kickstart does nothing to a job launchd is tearing down, so let the teardown finish
        // and see whether the job is still loaded afterwards.
        let state = match service.loaded_state()? {
            Some(state) if state.is_stopping() => {
                service.wait_while("stopping", state.teardown_bound(), LoadedState::is_stopping)?
            }
            state => state,
        };
        if state.is_some() {
            return service.kickstart();
        }

        lift_disabled(&service.plist_path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => not_installed(&ctx.label),
            _ => err,
        })?;
        // A concurrent start() can load the job first, which fails this bootstrap with EIO, and a
        // concurrent uninstall() can remove the plist, so the state afterwards decides.
        let bootstrapped = service.bootstrap();
        match service.loaded_state()? {
            // RunAtLoad and KeepAlive spawn the job during bootstrap, and kickstarting a job that
            // already ran and exited would run it a second time.
            Some(state) if state.spawned => Ok(()),
            Some(_) => service.kickstart(),
            None if !service.plist_path.try_exists()? => Err(not_installed(&ctx.label)),
            None => bootstrapped,
        }
    }

    /// Stops a service by removing it from its launchd domain, so KeepAlive cannot restart it.
    ///
    /// The plist stays in place, so launchd loads the service again at the next boot or login
    /// unless the plist sets `Disabled`.
    fn stop(&self, ctx: ServiceStopCtx) -> io::Result<()> {
        let service = self.service(&ctx.label)?;
        if service.bootout()? || service.plist_path.try_exists()? {
            return Ok(());
        }

        Err(not_installed(&ctx.label))
    }

    fn level(&self) -> ServiceLevel {
        if self.user {
            ServiceLevel::User
        } else {
            ServiceLevel::System
        }
    }

    fn set_level(&mut self, level: ServiceLevel) -> io::Result<()> {
        match level {
            ServiceLevel::System => self.user = false,
            ServiceLevel::User => self.user = true,
        }

        Ok(())
    }

    fn status(&self, ctx: crate::ServiceStatusCtx) -> io::Result<crate::ServiceStatus> {
        let service = self.service(&ctx.label)?;
        match service.loaded_state()? {
            Some(state) if state.is_running() => Ok(crate::ServiceStatus::Running),
            Some(_) => Ok(crate::ServiceStatus::Stopped(None)),
            None if service.plist_path.try_exists()? => Ok(crate::ServiceStatus::Stopped(None)),
            None => Ok(crate::ServiceStatus::NotInstalled),
        }
    }
}

impl LaunchdService {
    /// Reads the service's state, or `None` when it is not loaded in its domain.
    fn loaded_state(&self) -> io::Result<Option<LoadedState>> {
        let args: [&OsStr; 2] = ["print".as_ref(), self.target.as_ref()];
        let output = launchctl(&args)?;
        if is_not_loaded(&output) {
            return Ok(None);
        }

        let output = check(&args, output)?;
        Ok(Some(LoadedState::parse(&String::from_utf8_lossy(
            &output.stdout,
        ))))
    }

    /// Removes the service from its domain and waits for launchd to finish tearing it down.
    ///
    /// Returns whether the service was loaded.
    fn bootout(&self) -> io::Result<bool> {
        // A job booted out while launchd is still exec'ing it either refuses the signal, failing
        // the bootout with EPERM, or is abandoned by launchd and outlives its service.
        const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

        let Some(state) = self.wait_while("spawning", SPAWN_TIMEOUT, LoadedState::is_spawning)?
        else {
            return Ok(false);
        };

        let args: [&OsStr; 2] = ["bootout".as_ref(), self.target.as_ref()];
        if is_not_loaded(&output) {
            return Ok(false);
        }
        if output.status.code() != Some(exit_code::EINPROGRESS) {
            check(&args, output)?;
        }

        // bootout returns once launchd has signalled the job, not once the job is gone, and a
        // bootstrap of the label fails with EIO until it is.
        self.wait_while("loaded", state.teardown_bound(), |_| true)?;
        Ok(true)
    }

    /// Polls the service's state while it is loaded and `busy`, failing once `bound` passes.
    ///
    /// Returns the first state that is not busy, `None` meaning not loaded.
    fn wait_while(
        &self,
        what: &str,
        bound: Duration,
        busy: impl Fn(&LoadedState) -> bool,
    ) -> io::Result<Option<LoadedState>> {
        const POLL_INTERVAL: Duration = Duration::from_millis(100);

        let started = Instant::now();
        loop {
            let state = self.loaded_state()?;
            if !state.as_ref().is_some_and(&busy) {
                return Ok(state);
            }
            if started.elapsed() >= bound {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} is still {what} after {}s", self.target, bound.as_secs()),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

/// The fields of `launchctl print <service-target>` this manager acts on.
struct LoadedState {
    /// launchd's state for the job, such as `running`, `spawn scheduled` or `SIGTERMed`.
    state: String,
    /// Whether launchd has spawned the job since it was bootstrapped.
    spawned: bool,
    /// How long launchd waits after SIGTERM before it SIGKILLs the job, or `None` for an
    /// ExitTimeOut of 0, which launchd.plist(5) calls infinite.
    exit_timeout: Option<Duration>,
}

impl LoadedState {
    /// Whether launchd has spawned the job's process and is still exec'ing its program.
    fn is_spawning(&self) -> bool {
        matches!(self.state.as_str(), "spawning" | "xpcproxy")
    }

    fn is_running(&self) -> bool {
        self.state == "running" || self.is_spawning()
    }

    /// Whether launchd has signalled the job to exit and has not reaped it yet.
    fn is_stopping(&self) -> bool {
        matches!(self.state.as_str(), "SIGTERMed" | "SIGKILLed")
    }

    /// How long launchd can take to tear the job down once it has been signalled.
    fn teardown_bound(&self) -> Duration {
        // launchd SIGKILLs a job still running ExitTimeOut after its SIGTERM, then needs a
        // moment to reap it. launchd.plist(5) calls an ExitTimeOut of 0 infinite, so that wait
        // gets a fixed bound.
        const REAP_MARGIN: Duration = Duration::from_secs(5);
        const NO_EXIT_TIMEOUT_BOUND: Duration = Duration::from_secs(30);

        self.exit_timeout.unwrap_or(NO_EXIT_TIMEOUT_BOUND) + REAP_MARGIN
    }
}

fn is_not_loaded(output: &Output) -> bool {
    output
        .status
        .code()
        .is_some_and(|code| exit_code::EINPROGRESS.contains(&code))
}

/// Fails, naming the command, when `launchctl <args>` exited non-zero.
fn check(args: &[&OsStr], output: Output) -> io::Result<Output> {
    let kind = match output.status.code() {
        Some(exit_code::EPERM) => io::ErrorKind::PermissionDenied,
        Some(code) if exit_code::NOT_LOADED.contains(&code) => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };

    wrap_output(output).map_err(|err| {
        let command: Vec<_> = args.iter().map(|arg| arg.to_string_lossy()).collect();
        io::Error::new(kind, format!("launchctl {}: {err}", command.join(" ")))
    })
}

/// Whether a plist sets `Disabled`, which launchd honours by refusing to load it.
///
/// A plist that does not parse counts as not disabled, and bootstrap reports it.
fn sets_disabled(plist: &[u8]) -> bool {
    Value::from_reader(io::Cursor::new(plist))
        .ok()
        .and_then(Value::into_dictionary)
        .and_then(|dict| dict.get("Disabled").and_then(Value::as_boolean))
        .unwrap_or(false)
}

/// Removes `Disabled = true` from the plist at `path`, so that bootstrap loads it.
fn lift_disabled(path: &Path) -> io::Result<()> {
    let mut plist = Value::from_reader(io::Cursor::new(std::fs::read(path)?))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let Some(dict) = plist.as_dictionary_mut() else {
        return Ok(());
    };
    if dict.get("Disabled").and_then(Value::as_boolean) != Some(true) {
        return Ok(());
    }
    dict.remove("Disabled");

    let mut buffer = Vec::new();
    plist.to_writer_xml(&mut buffer).map_err(io::Error::other)?;
    // Not created afresh, so a plist a concurrent uninstall() removed stays removed.
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(&buffer)?;
    file.sync_all()
}

fn not_installed(label: &ServiceLabel) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("Service {} is not installed", label.to_qualified_name()),
    )
}

#[cfg(target_os = "macos")]
fn current_uid() -> io::Result<u32> {
    Ok(rustix::process::getuid().as_raw())
}

#[cfg(not(target_os = "macos"))]
fn current_uid() -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "launchd user domains exist only on macOS",
    ))
}

fn make_plist<'a>(
    config: &LaunchdInstallConfig,
    label: &str,
    args: impl Iterator<Item = &'a OsStr>,
    username: Option<String>,
    working_directory: Option<PathBuf>,
    environment: Option<Vec<(String, String)>>,
    autostart: bool,
    restart_policy: RestartPolicy,
) -> String {
    let mut dict = Dictionary::new();

    dict.insert("Label".to_string(), Value::String(label.to_string()));

    let program_arguments: Vec<Value> = args
        .map(|arg| Value::String(arg.to_string_lossy().into_owned()))
        .collect();
    dict.insert(
        "ProgramArguments".to_string(),
        Value::Array(program_arguments),
    );

    // Handle restart configuration
    // Priority: launchd-specific config > generic RestartPolicy
    if let Some(keep_alive) = config.keep_alive {
        // Use launchd-specific keep_alive configuration
        if keep_alive {
            dict.insert("KeepAlive".to_string(), Value::Boolean(true));
        }
    } else {
        // Fall back to generic RestartPolicy
        // Convert generic `RestartPolicy` to Launchd `KeepAlive`.
        //
        // Right now we are only supporting binary restart for Launchd, with `KeepAlive` either set or
        // not.
        //
        // However, Launchd does support further options when `KeepAlive` is set, e.g.,
        // `SuccessfulExit`. These could be extensions for the future.
        match restart_policy {
            RestartPolicy::Never => {
                // Don't set KeepAlive
            }
            RestartPolicy::Always { delay_secs } => {
                // KeepAlive *without* the SuccessfulExit construct will keep the service alive
                // whether the process exits successfully or not.
                dict.insert("KeepAlive".to_string(), Value::Boolean(true));
                if delay_secs.is_some() {
                    log::warn!(
                        "Launchd does not support restart delays; delay_secs will be ignored for service '{}'",
                        label
                    );
                }
            }
            RestartPolicy::OnFailure {
                delay_secs,
                max_retries: _,      // TODO: no direct launchd equivalent
                reset_after_secs: _, // TODO: no direct launchd equivalent
            } => {
                // Create KeepAlive dictionary with SuccessfulExit=false
                // This means: restart when exit is NOT successful
                let mut keep_alive_dict = Dictionary::new();
                keep_alive_dict.insert("SuccessfulExit".to_string(), Value::Boolean(false));
                dict.insert("KeepAlive".to_string(), Value::Dictionary(keep_alive_dict));

                if delay_secs.is_some() {
                    log::warn!(
                        "Launchd does not support restart delays; delay_secs will be ignored for service '{}'",
                        label
                    );
                }
            }
            RestartPolicy::OnSuccess { delay_secs } => {
                // Create KeepAlive dictionary with SuccessfulExit=true
                // This means: restart when exit is successful
                let mut keep_alive_dict = Dictionary::new();
                keep_alive_dict.insert("SuccessfulExit".to_string(), Value::Boolean(true));
                dict.insert("KeepAlive".to_string(), Value::Dictionary(keep_alive_dict));

                if delay_secs.is_some() {
                    log::warn!(
                        "Launchd does not support restart delays; delay_secs will be ignored for service '{}'",
                        label
                    );
                }
            }
        }
    }

    if let Some(username) = username {
        dict.insert("UserName".to_string(), Value::String(username));
    }

    if let Some(working_dir) = working_directory {
        dict.insert(
            "WorkingDirectory".to_string(),
            Value::String(working_dir.to_string_lossy().to_string()),
        );
    }

    if let Some(env_vars) = environment {
        let env_dict: Dictionary = env_vars
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        dict.insert(
            "EnvironmentVariables".to_string(),
            Value::Dictionary(env_dict),
        );
    }

    if autostart {
        dict.insert("RunAtLoad".to_string(), Value::Boolean(true));
    } else {
        dict.insert("RunAtLoad".to_string(), Value::Boolean(false));
    }

    let has_keep_alive = if let Some(keep_alive) = config.keep_alive {
        keep_alive
    } else {
        !matches!(restart_policy, RestartPolicy::Never)
    };

    // Set Disabled key to prevent the service automatically starting on load when KeepAlive is present.
    // This provides consistent cross-platform behaviour which is much more intuitive.
    // The service must be explicitly started via start().
    if has_keep_alive {
        dict.insert("Disabled".to_string(), Value::Boolean(true));
    }

    let plist = Value::Dictionary(dict);

    let mut buffer = Vec::new();
    plist.to_writer_xml(&mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;

    fn generated_plist(restart_policy: RestartPolicy) -> String {
        make_plist(
            &LaunchdInstallConfig::default(),
            "com.example.app",
            [OsStr::new("/bin/sleep")].into_iter(),
            None,
            None,
            None,
            true,
            restart_policy,
        )
    }

    // Checked through service() alone: if the guard regressed, stop() or uninstall() would boot
    // out the developer's whole domain.
    #[test]
    fn service_rejects_labels_launchctl_or_the_filesystem_would_misread() {
        let too_long = "x".repeat(250);
        for label in [
            "",
            "/",
            "com.example/other",
            "com.example.app\n\tstate = running",
            &too_long,
        ] {
            let err = LaunchdServiceManager::system()
                .service(&label.parse().unwrap())
                .err()
                .unwrap_or_else(|| panic!("label {label:?} was accepted"));
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "label {label:?}");
        }
    }

    #[test]
    fn loaded_state_ignores_fields_of_nested_dictionaries() {
        let state = LoadedState::parse(
            "gui/501/com.example.app = {\n\
             \tstate = not running\n\
             \texit timeout = 40\n\
             \tresource coalition = {\n\
             \t\tstate = running\n\
             \t\truns = 7\n\
             \t}\n\
             \truns = 0\n\
             }\n",
        );

        assert!(!state.is_running());
        assert!(!state.spawned);
        assert_eq!(state.exit_timeout, Some(Duration::from_secs(40)));
    }

    #[test]
    fn loaded_state_ignores_fields_forged_by_an_environment_value() {
        let state = LoadedState::parse(
            "gui/501/com.example.app = {\n\
             \tstate = not running\n\
             \tenvironment = {\n\
             \t\tAPP_CONFIG => [retry]\n\
             \truns = 3\n\
             \t}\n\
             \truns = 0\n\
             }\n",
        );

        assert!(!state.spawned);
    }

    #[test]
    fn loaded_state_treats_a_job_being_execed_as_running() {
        let state = LoadedState::parse(
            "gui/501/com.example.app = {\n\
             \tstate = xpcproxy\n\
             \texit timeout = 0\n\
             \truns = 1\n\
             }\n",
        );

        assert!(state.is_running());
        assert!(state.spawned);
        assert_eq!(state.exit_timeout, None);
    }

    #[test]
    fn generated_plist_is_disabled_only_for_keep_alive_services() {
        assert!(sets_disabled(
            generated_plist(RestartPolicy::default()).as_bytes()
        ));
        assert!(!sets_disabled(
            generated_plist(RestartPolicy::Never).as_bytes()
        ));
    }

    #[test]
    fn lift_disabled_rewrites_only_a_disabled_plist() {
        let dir = assert_fs::TempDir::new().unwrap();

        let disabled = dir.child("disabled.plist");
        disabled
            .write_str(&generated_plist(RestartPolicy::default()))
            .unwrap();
        lift_disabled(disabled.path()).unwrap();
        let lifted = std::fs::read(disabled.path()).unwrap();
        assert!(!sets_disabled(&lifted));
        assert!(String::from_utf8(lifted).unwrap().contains("KeepAlive"));

        let enabled = dir.child("enabled.plist");
        let contents = generated_plist(RestartPolicy::Never);
        enabled.write_str(&contents).unwrap();
        lift_disabled(enabled.path()).unwrap();
        enabled.assert(contents.as_str());

        let missing = lift_disabled(&dir.path().join("missing.plist")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}
