use super::*;
use std::str::Utf8Error;

/// Core interface to interact with `launchctl`.
pub(super) struct LaunchCtl {
    cmd: Command,
}

/// Errors thrown when trying to boot out a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchCtlBootoutError {
    /// There was a timeout attempting to boot out the service.
    Timeout,
    // There was an unexpected IO error booting out the service.
    Io(std::io::Error),
}

impl Error for LaunchCtlBootoutError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            LaunchCtlBootoutError::Timeout => None,
            LaunchCtlBootoutError::Io(io) => Some(io),
        }
    }
}

impl Display for LaunchCtlBootoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchCtlBootoutError::Timeout => write!(f, "unexpected timeout waiting for bootout"),
            LaunchCtlBootoutError::Io(_) => write!(f, "unexpected io error"),
        }
    }
}

/// Represents the state of the launchctl service.
///
/// These are all exposed by `launchctl print`.
pub enum ServiceState {
    Spawning,
    XpcProxy,
    Running,
    Sigtermed,
    Sigkilled,
    Other(String),
}

impl ServiceState {
    /// Whether the service is currently booting up or not.
    ///
    /// After the service has booted up and is running, it will no longer return true for this.
    pub fn is_booting(self) -> bool {
        matches!(self, ServiceState::Spawning | ServiceState::XpcProxy)
    }

    /// Whether the job has a process, counting one launchd has forked and is still exec'ing.
    ///
    /// Returns true either when the service is [`Self::is_booting`] or it's running.
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            ServiceState::Running | ServiceState::Spawning | ServiceState::XpcProxy
        )
    }

    /// Whether launchd has signalled the job to exit and has not reaped it yet.
    pub fn is_stopping(&self) -> bool {
        matches!(self, ServiceState::Sigtermed | ServiceState::Sigkilled)
    }
}

impl FromStr for ServiceState {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "spawning" => ServiceState::Spawning,
            "xpcproxy" => ServiceState::XpcProxy,
            "running" => ServiceState::Running,
            "SIGTERMed" => ServiceState::Sigtermed,
            "SIGKILLed" => ServiceState::Sigkilled,
            other => ServiceState::Other(other.into()),
        })
    }
}

/// The output of the `launchctl print` command.
pub struct ServicePrintout {
    /// The currently reported state of the service.
    pub state: ServiceState,
    /// How many runs the service had.
    pub runs: u64,
    /// How long launchd waits after SIGTERM before it SIGKILLs the job, if set.
    ///
    /// Note this can be infinite, in which case it is [`Option::None`].
    pub exit_timeout: Option<Duration>,
}

/// Why the output of `launchctl print <service-target>` could not be read.
#[derive(Debug)]
pub enum ServicePrintoutParseError {
    /// The output is not a UTF8 byte array.
    NotUtf8(Utf8Error),
    /// The service's dictionary has no such field.
    MissingField(&'static str),
    /// The field appears more than once in the service's own dictionary.
    ///
    /// Likely a parsing error, and you should probably report this.
    DuplicateField(&'static str),
    /// Lines follow the service's closing brace, so a printed value closed it early.
    ///
    /// This likely means something was misparsed and you should consider opening an issue.
    TrailingLines,
    /// The field's value could not be parsed.
    ParsingError {
        field: &'static str,
        source: Box<dyn Error + Send + Sync>,
    },
}

impl Error for ServicePrintoutParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ServicePrintoutParseError::NotUtf8(err) => Some(err),
            ServicePrintoutParseError::ParsingError { source, .. } => Some(source.as_ref()),
            ServicePrintoutParseError::MissingField(_)
            | ServicePrintoutParseError::DuplicateField(_)
            | ServicePrintoutParseError::TrailingLines => None,
        }
    }
}

impl Display for ServicePrintoutParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServicePrintoutParseError::NotUtf8(err) => {
                write!(f, "print output is not UTF-8: {err}")
            }
            ServicePrintoutParseError::MissingField(field) => {
                write!(f, "print output has no `{field}` field")
            }
            ServicePrintoutParseError::DuplicateField(field) => {
                write!(f, "print output has more than one `{field}` field")
            }
            ServicePrintoutParseError::TrailingLines => {
                write!(f, "trailing lines detected. likely misparsed")
            }
            ServicePrintoutParseError::ParsingError { field, .. } => {
                write!(f, "print field `{field}` could not be parsed")
            }
        }
    }
}

impl From<Utf8Error> for ServicePrintoutParseError {
    fn from(err: Utf8Error) -> Self {
        ServicePrintoutParseError::NotUtf8(err)
    }
}

impl ServicePrintout {
    /// The `key = value` lines of the service's dictionary, in printed order.
    fn own_fields(
        printout: &str,
    ) -> impl Iterator<Item = Result<(&str, &str), ServicePrintoutParseError>> + '_ {
        let mut lines = printout.lines();
        let mut depth = 0;
        std::iter::from_fn(move || loop {
            let line = lines.next()?;
            let text = line.trim_start_matches('\t');
            let indent = line.len() - text.len();
            if text == "}" && indent + 1 == depth {
                depth -= 1;
                if depth > 0 {
                    continue;
                }
                // The service's closing brace is the last line, unless a value forged it. Either
                // way nothing after it is a field.
                let trailing = lines.any(|line| !line.is_empty());
                lines = "".lines();
                return trailing.then_some(Err(ServicePrintoutParseError::TrailingLines));
            }
            if indent != depth {
                continue;
            }
            if text.ends_with(" = {") {
                depth += 1;
                continue;
            }
            if depth != 1 {
                continue;
            }
            if let Some(field) = text.split_once(" = ") {
                return Some(Ok(field));
            }
        })
    }

    /// The service's own `key` field, parsed as `T`.
    fn field<T>(printout: &str, key: &'static str) -> Result<T, ServicePrintoutParseError>
    where
        T: FromStr,
        T::Err: Error + Send + Sync + 'static,
    {
        let mut found = None;
        for field in Self::own_fields(printout) {
            let (name, value) = field?;
            if name != key {
                continue;
            }

            if found.replace(value).is_some() {
                return Err(ServicePrintoutParseError::DuplicateField(key));
            }
        }

        found
            .ok_or(ServicePrintoutParseError::MissingField(key))?
            .parse()
            .map_err(|source| ServicePrintoutParseError::ParsingError {
                field: key,
                source: Box::new(source),
            })
    }
}

impl TryFrom<&[u8]> for ServicePrintout {
    type Error = ServicePrintoutParseError;

    /// Try to parse the output of `launchctl print`.
    ///
    /// Please note `launchctl print`'s output is not stable and Apple **EXPLICITLY** says it's not
    /// stable but we have no other option.
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        // Apple's print format mixes `key = value` fields, nested `key = {` dictionaries,
        // `KEY => value` environment entries and bare lines. We need three fields of the
        // service's own dictionary:
        //
        // gui/501/2BUA8C4S2C.com.1password.browser-helper = {
        //         state = running
        //         ...
        //         exit timeout = 5
        //         runs = 1
        //         ...
        // }
        let printout = std::str::from_utf8(value)?;

        let state: ServiceState = Self::field(printout, "state")?;
        let runs: u64 = Self::field(printout, "runs")?;
        let exit_timeout = match Self::field::<u64>(printout, "exit timeout")? {
            0 => None,
            secs => Some(Duration::from_secs(secs)),
        };

        Ok(Self {
            state,
            runs,
            exit_timeout,
        })
    }
}

impl LaunchCtl {
    const LAUNCHCTL_PATH: &'static str = "/bin/launchctl";

    /// `launchctl` exit codes for a service that is not loaded: `ESRCH` from `bootout`, then
    /// "Could not find specified domain" and "Could not find specified service".
    pub const NOT_LOADED: [i32; 3] = [3, 112, 113];

    /// `launchctl` exit code for `EPERM`, which a non-root caller gets for the `system` domain.
    pub const EPERM: i32 = 1;

    /// `launchctl bootout` exit code for `EINPROGRESS`, returned while the job is still exiting.
    pub const EINPROGRESS: i32 = 36;

    pub fn available() -> bool {
        Path::new(Self::LAUNCHCTL_PATH).exists()
    }

    /// `launchctl bootstrap [service-path service-path2 ...]`
    pub fn bootstrap_plists(
        domain: ServiceDomain,
        plists: impl Iterator<Item = impl AsRef<Path>>,
    ) -> io::Result<process::Output> {
        Self::start()
            .arg("bootstrap")
            .arg(domain.into())
            .args(plists)
            .output()
    }

    /// `launchctl bootout service-target`.
    ///
    /// Waits for the launchd to finish tearing out the service. Please note this function can
    /// block the active thread until the requested service is booted out.
    pub fn bootout(
        domain: ServiceDomain,
        service: &str,
        timeout: Duration,
    ) -> Result<(), LaunchCtlBootoutError> {
        let procedure_start_time = Instant::now();

        const BOOTUP_TIMEOUT: Duration = Duration::from_secs(1);
        const POLLING_PERIOD: Duration = Duration::from_millis(100);

        // A process may be in progress of being booted in. We have to make sure it's fully loaded
        // before we try to bootout otherwise bootout will yell at us.
        let started = Instant::now();
        let mut status: ServicePrintout;
        loop {
            status = Self::print_service(domain, service)?;

            let now = Instant::now();
            if !status.state.is_booting() {
                break;
            } else if now - procedure_start_time >= timeout {
                return Err(LaunchCtlBootoutError::Timeout);
            } else if now - started > BOOTUP_TIMEOUT {
                return Err(LaunchCtlBootoutError::Timeout);
            }

            std::thread::sleep(POLLING_PERIOD);
        }

        // Normally launchctl tells us how many seconds before it sigkills the service. We add an
        // extra second of leeway to make sure the sigkill goes through
        let bootout_timeout = status.exit_timeout + Duration::from_secs(1);

        // Well now we now that the service is not in the booting up state, we can try to boot it
        // out.
        //
        // We call output_raw here since launchctl will return non-zero exit codes and
        // [`Self::output`] would abort on those.
        let output = Self::start().arg("bootout").arg(service).output_raw()?;

        // We then have to block until we can confirm that the service actually goes down.
        let started = Instant::now();
        loop {
            if Self::start()
                .arg("print")
                .arg(format!("{}/{service}", domain.into()))
                .output_raw()?
                .status
                .code()
                // TODO(markovejnovic): Make a const for this.
                .is_some_and(|c| c == 113)
            {
                break;
            }

            let now = Instant::now();
            if now - procedure_start_time >= timeout {
                return Err(LaunchCtlBootoutError::Timeout);
            } else if now - started > BOOTUP_TIMEOUT {
                return Err(LaunchCtlBootoutError::Timeout);
            }
        }

        Ok(())
    }

    /// `launchctl kickstart service-target`
    pub fn kickstart(service: &str) -> io::Result<process::Output> {
        Self::start().arg("kickstart").arg(service).output()
    }

    /// `launchctl print service-target`
    ///
    /// **Warning**: Apple explicitly documents this API as not being stable and safe to use, but
    /// unfortunately, I couldn't find a different, more appropriate API. We use this API, but we
    /// keep the surface of what we read very minimal.
    pub fn print_service(
        domain: ServiceDomain,
        service: &str,
    ) -> Result<ServicePrintout, ServicePrintoutError> {
        let stdout = Self::start()
            .arg("print")
            .arg(format!("{}/{service}", domain.into()))
            .output()?
            .stdout;

        ServicePrintout::try_from(stdout)
    }

    fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.cmd.arg(arg);
        Self { ..self }
    }

    fn args(mut self, args: impl Iterator<Item = impl AsRef<OsStr>>) -> Self {
        self.cmd.args(args);
        Self { ..self }
    }

    fn output_raw(mut self) -> io::Result<std::process::Output> {
        self.cmd.output()
    }

    /// Run a command, parsing the exit code.
    fn output(self) -> std::io::Result<std::process::Output> {
        let output = self.cmd.output()?;

        wrap_output(output).map_err(|err| {
            let kind = match output.status.code() {
                Some(EPERM) => io::ErrorKind::PermissionDenied,
                Some(code) if NOT_LOADED.contains(&code) => io::ErrorKind::NotFound,
                _ => io::ErrorKind::Other,
            };

            std::io::Error::new(kind, format!("{}: {err}", self.cmd.into()))
        })
    }

    fn start() -> Self {
        let mut cmd = Command::new(Self::LAUNCHCTL_PATH);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Self { cmd }
    }
}
