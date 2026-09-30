//! Starting the container's moving parts and keeping them up.
//!
//! This is the whole container: the browser install, the screen, the window
//! manager, the VNC server, and the web service that lets a browser watch the
//! screen and a tool drive it. Whichever part stops first brings the container
//! down — a half-running desktop is worse than one that restarts and comes
//! back whole.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{
    process::{Child, Command},
    signal,
    sync::mpsc,
    time::{sleep, timeout},
};
use tracing::{debug, error, info, warn};

use crate::{
    config::{Brave, Config, Session},
    web,
};

/// The home the image gives the container's user, and a place inside it for
/// per-run state that is not worth a volume.
const HOME: &str = "/home/headless";
const RUNTIME_DIR: &str = "/home/headless/.x11vnc";

/// The browser's repository, as a `.list` file, and the key that signs it.
/// Both are baked into the image.
const SOURCES_LIST: &str = "/etc/apt/sources.list.d/brave-browser-release.list";
const SIGNING_KEY: &str = "/usr/share/keyrings/brave-browser-archive-keyring.gpg";

/// How often the children are checked. A process dying is noticed within this,
/// which is quicker than anyone can notice, and keeps the children killable.
const POLL: Duration = Duration::from_millis(200);

/// How long a child is given to exit on SIGTERM before it is killed. The
/// browser gets the time to close its profile, which is the difference between
/// a clean shutdown and a "didn't shut down correctly" on the next start.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// A process this supervisor started, and what to call it in a log line.
struct Service {
    name: &'static str,
    child: Child,
}

enum Outcome {
    /// Someone asked us to stop.
    Stopped(&'static str),
    /// Something we depend on is gone.
    Failed(String),
}

/// Runs the container until something stops working, then stops everything.
pub async fn run(config: Config) -> Result<()> {
    prepare(&config).await?;

    let mut services = Vec::new();
    services.push(start("Xvfb", xvf(&config))?);
    wait_for_screen(&config.session).await?;
    services.push(start("fluxbox", fluxbox(&config))?);
    services.push(start("Brave", brave(&config))?);
    services.push(x11vnc(&config).await?);

    // The web service is not a child: it is this process, with the children
    // around it. So it is a task, and its failing is ours failing.
    let (finished, mut web_done) = mpsc::unbounded_channel();
    let config = Arc::new(config);
    let serving = serve(Arc::clone(&config)).await?;
    let web = tokio::spawn(async move {
        if let Err(error) = serving.await {
            error!(%error, "the web service stopped");
        }
        let _ = finished.send(());
    });

    // Nothing else may stop the browser, so an update arrives as a message
    // rather than as an action taken behind the supervisor's back.
    let (updates, mut pending) = mpsc::unbounded_channel();
    let keeping = tokio::spawn(keep_current(config.session.brave.clone(), updates));

    let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate())?;
    let mut interrupt = signal::unix::signal(signal::unix::SignalKind::interrupt())?;

    let outcome = loop {
        if let Some(dead) = first_to_exit(&mut services) {
            break Outcome::Failed(dead);
        }
        tokio::select! {
            Some(package) = pending.recv() => {
                if let Err(error) = replace_browser(&mut services, &config, &package).await {
                    error!(%error, "the update failed");
                    break Outcome::Failed(format!("the update: {error}"));
                }
            }
            _ = web_done.recv() => break Outcome::Failed("the web service".to_owned()),
            _ = terminate.recv() => break Outcome::Stopped("terminated"),
            _ = interrupt.recv() => break Outcome::Stopped("interrupted"),
            () = sleep(POLL) => {}
        }
    };

    info!("stopping");
    keeping.abort();
    web.abort();
    shutdown(&mut services).await;
    match outcome {
        Outcome::Stopped(why) => {
            info!(%why, "stopped");
            Ok(())
        }
        Outcome::Failed(who) => {
            // The restart policy takes it from here, and the code says why.
            Err(anyhow::anyhow!("{who} exited"))
        }
    }
}

fn first_to_exit(services: &mut [Service]) -> Option<String> {
    // A child we cannot ask about yet is not a child that has gone.
    services
        .iter_mut()
        .find_map(|service| match service.child.try_wait() {
            Ok(Some(status)) => Some(format!("{} ({status})", service.name)),
            Ok(None) | Err(_) => None,
        })
}

/// Asks a child to stop, then insists.
async fn stop(service: &mut Service) {
    if let Some(pid) = service.child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: sending a signal to a pid this process started, which is
        // still ours to signal because it has not been reaped.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }

    let gone = async {
        loop {
            if matches!(service.child.try_wait(), Ok(Some(_)) | Err(_)) {
                return;
            }
            sleep(POLL).await;
        }
    };
    // A child still there after the grace period stops being asked.
    let _ = timeout(SHUTDOWN_GRACE, gone).await;

    let _ = service.child.start_kill();
    let _ = service.child.wait().await;
}

/// Asks every child to stop, then insists.
async fn shutdown(services: &mut [Service]) {
    for service in services.iter_mut() {
        stop(service).await;
    }
}

/// Everything that has to be true of the filesystem before anything starts.
async fn prepare(config: &Config) -> Result<()> {
    let session = &config.session;

    create(&session.profile, "the browser profile").await?;
    create(Path::new(RUNTIME_DIR), "the x11vnc run directory").await?;
    create(&screen_directory(), "the X socket directory").await?;
    make_shared(&screen_directory())?;

    // A restart that killed the previous Xvfb leaves its display locked, and
    // the next one refuses to start over a lock nobody is holding.
    remove(&format!("/tmp/.X{}-lock", session.display));
    remove(&socket_path(session).display().to_string());

    // A profile that outlives the container keeps Chromium's single-instance
    // lock, which names a process that no longer exists. Left alone, Brave
    // refuses to start and the container comes up with no browser at all.
    for name in ["Lock", "Cookie", "Socket"] {
        remove(
            &session
                .profile
                .join(format!("Singleton{name}"))
                .display()
                .to_string(),
        );
    }

    install_brave(&session.brave).await?;
    Ok(())
}

fn screen_directory() -> PathBuf {
    PathBuf::from("/tmp/.X11-unix")
}

fn socket_path(session: &Session) -> PathBuf {
    screen_directory().join(format!("X{}", session.display))
}

async fn create(path: &Path, what: &str) -> Result<()> {
    if let Err(error) = tokio::fs::create_dir_all(path).await {
        bail!("cannot create {what} at {}: {error}", path.display());
    }
    Ok(())
}

/// Makes a directory usable by everyone, but only when it is ours to change: an
/// X server that cannot create its socket stops every client, including us.
fn make_shared(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.mode() & 0o777 == 0o777 {
        return Ok(());
    }
    if meta.uid() != std::process::id() {
        debug!(path = %path.display(), "leaving the mode of a directory we do not own");
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777))
        .with_context(|| format!("cannot make {} usable by all", path.display()))
}

fn remove(path: &str) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            info!(%path, %error, "could not clear a stale file");
        }
    }
}

/// Waits for the X server to be there before anything tries to draw on it.
async fn wait_for_screen(session: &Session) -> Result<()> {
    let path = socket_path(session);
    let appeared = async {
        while !path.exists() {
            sleep(Duration::from_millis(100)).await;
        }
    };
    timeout(Duration::from_secs(10), appeared)
        .await
        .map_err(|_| anyhow::anyhow!("no X server on display :{}", session.display))?;
    Ok(())
}

/// Installs the browser when the volume it lives on is empty. Once it is there,
/// keeping it current is the ticker's business, not this one's.
async fn install_brave(brave: &Brave) -> Result<()> {
    if brave.binary().is_file() {
        return Ok(());
    }
    info!(root = %brave.root.display(), "installing the browser");
    apply_package(brave, &download_package().await?).await?;
    if !brave.binary().is_file() {
        bail!("the browser is not installed at {}", brave.root.display());
    }
    Ok(())
}

/// The version installed on the volume, recorded when it was unpacked: there
/// is no dpkg database in here to ask.
fn installed_brave_version(brave: &Brave) -> Option<String> {
    std::fs::read_to_string(brave.root.join("VERSION"))
        .ok()
        .map(|version| version.trim().to_owned())
}

/// Unpacks a downloaded package over the install directory.
///
/// No root, and deliberately not `apt-get install`: the container runs
/// unprivileged, the image records the package as installed with its payload
/// deleted, and apt's plan for reinstalling it is not something to rely on.
/// `dpkg-deb --extract` is an unpack, not an install.
///
/// The browser must already be stopped when this runs: it deletes the install
/// directory, which a running browser still has open.
async fn apply_package(brave: &Brave, package: &Path) -> Result<()> {
    // The install directory is usually a volume, which is a different
    // filesystem from the temporary directory and so cannot be renamed onto.
    // Unpacking and moving both happen inside it instead.
    let parent = brave
        .root
        .parent()
        .with_context(|| format!("{} has no directory to stage in", brave.root.display()))?
        .to_path_buf();
    let staging = parent.join(".headless-brave-staging");
    let _ = tokio::fs::remove_dir_all(&staging).await;
    create(&staging, "the staging directory").await?;
    capture(
        Command::new("dpkg-deb")
            .arg("--extract")
            .arg(package)
            .arg(&staging),
        "dpkg-deb --extract",
    )
    .await?;

    // The package unpacks to ./opt/brave.com/...; only that subtree is ours to
    // keep, because the libraries it links against are already in the image.
    let unpacked = staging.join("opt").join("brave.com");
    if !unpacked.join("brave").join("brave").is_file() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        bail!("the browser is not in the package");
    }
    let target = brave.root.clone();
    let _ = tokio::fs::remove_dir_all(&target).await;
    if let Err(error) = tokio::fs::rename(&unpacked, &target).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error)
            .with_context(|| format!("cannot move the browser into {}", target.display()));
    }
    tokio::fs::write(target.join("VERSION"), package_version(package))
        .await
        .context("cannot record the version that was installed")?;

    let _ = tokio::fs::remove_dir_all(&staging).await;
    Ok(())
}

/// Downloads the browser package into a directory of its own, so that it can
/// wait there until the browser is stopped and unpacking it is safe.
async fn download_package() -> Result<PathBuf> {
    let work = std::env::temp_dir().join("headless-brave-download");
    let keep = std::env::temp_dir().join("headless-brave-package");
    let _ = tokio::fs::remove_dir_all(&work).await;
    let _ = tokio::fs::remove_dir_all(&keep).await;
    for directory in ["apt/lists/partial", "package"] {
        create(&work.join(directory), "the work directory").await?;
    }

    capture(
        apt("apt-get", &work)
            .current_dir(work.join("package"))
            .arg("update"),
        "apt-get update",
    )
    .await?;
    capture(
        apt("apt-get", &work)
            .current_dir(work.join("package"))
            .arg("download")
            .arg("brave-browser"),
        "apt-get download",
    )
    .await?;
    let package = find_package(&work.join("package"))
        .await
        .with_context(|| "the browser package could not be fetched")?;

    create(&keep, "the download directory").await?;
    let kept = keep.join(
        package
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("brave-browser.deb")),
    );
    tokio::fs::rename(&package, &kept)
        .await
        .with_context(|| "cannot keep the downloaded package")?;
    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(kept)
}

/// Whatever apt left in the download directory: it names the file it saved.
async fn find_package(directory: &Path) -> Option<PathBuf> {
    let mut paths = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return None;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        paths.push(entry.path());
    }
    paths.sort();
    paths.into_iter().find(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("deb"))
    })
}

/// The version out of a package filename, e.g. `brave-browser_1.2.3_amd64.deb`.
fn package_version(package: &Path) -> String {
    package
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .and_then(|name| {
            // brave-browser_1.2.3_amd64.deb: the version is the middle field.
            let mut fields = name.trim_end_matches(".deb").splitn(3, '_');
            fields.next()?;
            fields.next().map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// An apt tool pointed at an index of its own, because the one in the image is
/// not ours to write.
fn apt(program: &str, work: &Path) -> Command {
    let mut apt = Command::new(program);
    apt.arg("-qq");
    for option in [
        format!("Dir::State={}", work.join("apt").display()),
        format!("Dir::Cache={}", work.join("apt").display()),
        format!("Dir::State::lists={}", work.join("apt/lists").display()),
        format!("Dir::Etc::sourcelist={SOURCES_LIST}"),
        "Dir::Etc::sourceparts=/dev/null".to_owned(),
        format!("Dir::Etc::trusted={SIGNING_KEY}"),
        "Dir::Etc::trustedparts=/dev/null".to_owned(),
    ] {
        apt.arg("-o").arg(option);
    }
    apt
}

/// How often to ask the repository whether the browser has moved on.
///
/// Brave ships security releases weekly at best, so the worst case here is how
/// long a known browser fix goes unpicked. The check is one small index fetch;
/// nothing is downloaded unless there is something new.
const UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Whether what is installed is behind what the repository offers.
///
/// A repository we could not ask is not a reason to reinstall: that would put
/// a download on every tick for as long as the network is away.
fn is_behind(installed: Option<&str>, newest: Option<&str>) -> bool {
    newest.is_some_and(|newest| installed != Some(newest))
}

/// Keeps the browser current without anyone asking: ask the repository on a
/// timer, and when it offers something newer, download it and hand it to the
/// supervisor, which is the only thing that may stop the browser.
async fn keep_current(brave: Brave, updates: mpsc::UnboundedSender<PathBuf>) {
    let mut ticker = tokio::time::interval(UPDATE_INTERVAL);
    loop {
        // Fires immediately, so a container that is restarted often is checked
        // often, and one that runs for months is checked anyway.
        ticker.tick().await;
        match prepare_update(&brave).await {
            Ok(Some(package)) => {
                info!("a newer browser is waiting to be installed");
                if updates.send(package).is_err() {
                    return;
                }
            }
            Ok(None) => debug!("the browser is current"),
            // An update that cannot be fetched leaves a working browser
            // alone; the next tick tries again.
            Err(error) => warn!(%error, "could not check for a newer browser"),
        }
    }
}

/// Downloads a newer browser if the repository has one, and says where it is.
async fn prepare_update(brave: &Brave) -> Result<Option<PathBuf>> {
    let newest = newest_brave_version().await?;
    if !is_behind(installed_brave_version(brave).as_deref(), newest.as_deref()) {
        return Ok(None);
    }
    info!(?newest, "downloading it");
    Ok(Some(download_package().await?))
}

/// Stops the browser, puts the new one in its place, and starts it again.
///
/// The screen, the window manager and both servers stay up, so the desktop a
/// viewer is watching survives: only the window on it goes away and comes back.
async fn replace_browser(services: &mut [Service], config: &Config, package: &Path) -> Result<()> {
    let at = services
        .iter()
        .position(|s| s.name == "Brave")
        .context("the browser is not running")?;
    let slot = services.get_mut(at).context("the browser is not running")?;
    info!("restarting the browser to install the update");
    stop(slot).await;
    apply_package(&config.session.brave, package).await?;
    let _ = tokio::fs::remove_dir_all(std::env::temp_dir().join("headless-brave-package")).await;
    *slot = start("Brave", brave(config))?;
    info!(
        version = %installed_brave_version(&config.session.brave).unwrap_or_default(),
        "installed"
    );
    Ok(())
}

/// The newest version the repository offers, if it will say. Its own copy of
/// the index, for the same reason the install has one.
async fn newest_brave_version() -> Result<Option<String>> {
    let work = std::env::temp_dir().join("headless-brave-check");
    let _ = tokio::fs::remove_dir_all(&work).await;
    if create(&work.join("apt/lists/partial"), "the work directory")
        .await
        .is_err()
    {
        return Ok(None);
    }
    if capture(apt("apt-get", &work).arg("update"), "apt-get update")
        .await
        .is_err()
    {
        let _ = tokio::fs::remove_dir_all(&work).await;
        return Ok(None);
    }
    let policy = capture(
        apt("apt-cache", &work).arg("policy").arg("brave-browser"),
        "apt-cache policy",
    )
    .await
    .ok();
    let _ = tokio::fs::remove_dir_all(&work).await;

    Ok(policy.and_then(|policy| {
        policy
            .lines()
            .find_map(|line| line.trim().strip_prefix("Candidate: "))
            .map(str::to_owned)
    }))
}

/// Runs a command, returns what it printed, and logs the whole exchange so a
/// failed install says why.
async fn capture(command: &mut Command, what: &str) -> Result<String> {
    let output = command
        .output()
        .await
        .with_context(|| format!("cannot run {what}"))?;
    debug!(
        %what,
        status = %output.status,
        "{}", String::from_utf8_lossy(&output.stdout)
    );
    if !output.status.success() {
        bail!(
            "{what} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn start(name: &'static str, mut command: Command) -> Result<Service> {
    let child = command
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot start {name}"))?;
    Ok(Service { name, child })
}

fn xvf(config: &Config) -> Command {
    let mut command = Command::new("Xvfb");
    command
        .arg(format!(":{}", config.session.display))
        .arg("-screen")
        .arg("0")
        .arg(format!("{}x24", config.session.resolution))
        .arg("-nolisten")
        .arg("tcp")
        .arg("-noreset");
    command
}

fn fluxbox(config: &Config) -> Command {
    let mut command = Command::new("fluxbox");
    command.env("DISPLAY", format!(":{}", config.session.display));
    command
}

fn brave(config: &Config) -> Command {
    let session = &config.session;
    let mut command = Command::new(session.brave.binary());
    command
        .env("DISPLAY", format!(":{}", session.display))
        .env("HOME", HOME)
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-features=Translate")
        // The container is stopped abruptly, so a profile that survives it
        // always looks like a crash. Without this, a "Restore pages?" bubble is
        // waiting on the shared desktop every time, covering whatever the
        // browser was doing.
        .arg("--hide-crash-restore-bubble")
        .arg(format!(
            "--window-size={},{}",
            session.resolution.width, session.resolution.height
        ))
        .arg(format!("--user-data-dir={}", session.profile.display()))
        .arg(format!("--remote-debugging-port={}", cdp_port(config)))
        .arg("about:blank");
    command
}

/// The browser's `DevTools` port, read back out of the endpoint we were given
/// so that the two cannot drift apart.
fn cdp_port(config: &Config) -> u16 {
    config
        .cdp_version_url
        .rsplit(':')
        .next()
        .and_then(|port| port.split('/').next())
        .and_then(|port| port.parse().ok())
        .unwrap_or(9224)
}

async fn x11vnc(config: &Config) -> Result<Service> {
    let password_file = Path::new(RUNTIME_DIR).join("passwd");
    // x11vnc only writes a password file on its own; the server then reads it.
    create(Path::new(RUNTIME_DIR), "the x11vnc run directory").await?;
    let file = password_file.display().to_string();
    capture(
        Command::new("x11vnc")
            .arg("-quiet")
            .arg("-storepasswd")
            .arg(&config.vnc.password)
            .arg(&file),
        "x11vnc -storepasswd",
    )
    .await?;

    let mut command = Command::new("x11vnc");
    command
        .arg("-display")
        .arg(format!(":{}", config.session.display))
        .arg("-rfbport")
        .arg(config.vnc.port.to_string())
        .arg("-rfbauth")
        .arg(password_file)
        // -shared: every viewer sees the same screen and none of them owns it.
        // -forever: keep serving after the last viewer disconnects, which is
        // the whole point — the browser is driven over CDP, not by whoever
        // happens to be looking.
        .arg("-forever")
        .arg("-shared")
        .arg("-repeat")
        .arg("-noxdamage");
    start("x11vnc", command)
}

/// Binds the two listeners and serves until the process goes away.
async fn serve(config: Arc<Config>) -> Result<impl Future<Output = Result<()>>> {
    use tokio::net::TcpListener;

    let web = TcpListener::bind(config.web_addr)
        .await
        .with_context(|| format!("cannot listen on {}", config.web_addr))?;
    let cdp = TcpListener::bind(config.cdp_addr)
        .await
        .with_context(|| format!("cannot listen on {}", config.cdp_addr))?;

    info!(
        web = %config.web_addr,
        cdp = %config.cdp_addr,
        vnc = config.vnc.port,
        browser = %config.cdp_version_url,
        "ready"
    );

    Ok(async move {
        tokio::try_join!(
            axum::serve(web, web::router(&config)).with_graceful_shutdown(until_the_end()),
            axum::serve(cdp, web::cdp_router(&config)).with_graceful_shutdown(until_the_end()),
        )?;
        Ok(())
    })
}

/// The web service serves until the process goes away, which is what stopping
/// the container means. Nothing shuts it down from inside.
async fn until_the_end() {
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_behind, package_version};

    #[test]
    fn reads_the_version_out_of_a_package_name() {
        assert_eq!(
            package_version(Path::new("/tmp/brave-browser_1.2.3_amd64.deb")),
            "1.2.3"
        );
    }

    #[test]
    fn copes_with_a_name_it_does_not_recognise() {
        assert_eq!(package_version(Path::new("brave.deb")), "unknown");
    }

    #[test]
    fn updates_when_the_repository_is_ahead() {
        assert!(is_behind(Some("1.96.58"), Some("1.96.59")));
        // Nothing recorded yet, so anything the repository offers is newer.
        assert!(is_behind(None, Some("1.96.59")));
    }

    #[test]
    fn leaves_it_alone_when_the_repository_has_nothing_new() {
        assert!(!is_behind(Some("1.96.59"), Some("1.96.59")));
    }

    #[test]
    fn leaves_it_alone_when_the_repository_cannot_be_asked() {
        // The one that matters: a network that is down must not turn every
        // tick into a reinstall of a browser that is already working.
        assert!(!is_behind(Some("1.96.59"), None));
        assert!(!is_behind(None, None));
    }
}
