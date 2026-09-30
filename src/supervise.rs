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
use tracing::{debug, error, info};

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

    let mut terminate = signal::unix::signal(signal::unix::SignalKind::terminate())?;
    let mut interrupt = signal::unix::signal(signal::unix::SignalKind::interrupt())?;

    let outcome = loop {
        if let Some(dead) = first_to_exit(&mut services) {
            break Outcome::Failed(dead);
        }
        tokio::select! {
            _ = web_done.recv() => break Outcome::Failed("the web service".to_owned()),
            _ = terminate.recv() => break Outcome::Stopped("terminated"),
            _ = interrupt.recv() => break Outcome::Stopped("interrupted"),
            () = sleep(POLL) => {}
        }
    };

    info!("stopping");
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

/// Asks every child to stop, then insists.
async fn shutdown(services: &mut [Service]) {
    for service in services.iter_mut() {
        if let Some(pid) = service.child.id().and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: sending a signal to a pid this process started, which
            // is still ours to signal because it has not been reaped.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }

    let everyone_left = async {
        loop {
            if services
                .iter_mut()
                .all(|s| matches!(s.child.try_wait(), Ok(Some(_)) | Err(_)))
            {
                return;
            }
            sleep(POLL).await;
        }
    };
    // Whoever is still there after the grace period stops being asked.
    let _ = timeout(SHUTDOWN_GRACE, everyone_left).await;

    for service in services.iter_mut() {
        let _ = service.child.start_kill();
    }
    for service in services.iter_mut() {
        let _ = service.child.wait().await;
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

/// Installs the browser when the volume it lives on is empty, or checks for a
/// newer release when asked to.
async fn install_brave(brave: &Brave) -> Result<()> {
    if brave.binary().is_file() {
        if !brave.upgrade {
            return Ok(());
        }
        // Ask whether there is anything newer, and take it if so. Off by
        // default because it costs a download on every start.
        info!("checking for a newer browser");
        match newest_brave_version().await? {
            Some(newest) if Some(&newest) == installed_brave_version(brave).as_ref() => {
                info!(%newest, "already on the newest release");
                return Ok(());
            }
            Some(newest) => info!(%newest, "a newer release is available"),
            // The update is an optimisation, not a requirement: an index we
            // could not fetch is no reason to reinstall on every start.
            None => {
                info!("the repository did not say; keeping what is installed");
                return Ok(());
            }
        }
    } else {
        info!(root = %brave.root.display(), "installing the browser");
    }

    unpack_brave(brave).await?;

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

/// Fetches the package and unpacks it over the install directory.
///
/// No root, and deliberately not `apt-get install`: the container runs
/// unprivileged, the image records the package as installed with its payload
/// deleted, and apt's plan for reinstalling it is not something to rely on.
/// `apt-get download` only needs somewhere writable to keep its index, and
/// `dpkg-deb --extract` is an unpack, not an install.
async fn unpack_brave(brave: &Brave) -> Result<()> {
    let work = std::env::temp_dir().join("headless-brave");
    let _ = tokio::fs::remove_dir_all(&work).await;
    for directory in ["apt/lists/partial", "package"] {
        create(&work.join(directory), "the work directory").await?;
    }

    let package = fetch_brave(&work).await?;

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
            .arg(&package)
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
    tokio::fs::write(target.join("VERSION"), package_version(&package))
        .await
        .context("cannot record the version that was installed")?;

    let _ = tokio::fs::remove_dir_all(&staging).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(())
}

/// Downloads the browser package, refreshing an index that is ours to write.
async fn fetch_brave(work: &Path) -> Result<PathBuf> {
    let package = work.join("package");
    capture(
        apt("apt-get", work).current_dir(&package).arg("update"),
        "apt-get update",
    )
    .await?;
    capture(
        apt("apt-get", work)
            .current_dir(&package)
            .arg("download")
            .arg("brave-browser"),
        "apt-get download",
    )
    .await?;
    find_package(&package)
        .await
        .with_context(|| "the browser package could not be fetched")
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

    use super::package_version;

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
}
