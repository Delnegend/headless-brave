//! Starting the container's moving parts and keeping them up.
//!
//! This is what the entrypoint script used to do. The browser, the screen, the
//! window manager and the VNC server are all started here, and whichever one
//! exits first brings the whole container down — a half-running desktop is
//! worse than a container that restarts itself and comes back whole.

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

/// How often the children are checked. A process dying is noticed within this,
/// which is quicker than anyone can notice and keeps the children killable.
const POLL: Duration = Duration::from_millis(200);

/// How long a child is given to exit on SIGTERM before it is killed. The browser
/// gets the time to close its profile, which is the difference between a clean
/// shutdown and a "didn't shut down correctly" on the next start.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// A process this supervisor started, and what to call it in a log line.
struct Service {
    name: &'static str,
    child: Child,
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
            () = tokio::time::sleep(POLL) => {}
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

enum Outcome {
    /// Someone asked us to stop.
    Stopped(&'static str),
    /// Something we depend on is gone.
    Failed(String),
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
    create(Path::new("/run/x11vnc"), "the x11vnc run directory").await?;
    create(&screen_directory(), "the X socket directory").await?;
    set_mode(&screen_directory(), 0o1777)
        .context("cannot make the X socket directory usable by all")?;

    // A restart that killed the previous Xvfb leaves its display locked, and
    // the next one refuses to start over a lock nobody is holding.
    let lock = format!("/tmp/.X{}-lock", session.display);
    remove(&lock);
    remove(&socket_path(session).display().to_string());

    // A profile that outlives the container keeps Chromium's single-instance
    // lock, which names a process that no longer exists. Left alone, Brave
    // refuses to start and the container comes up with no browser at all.
    for name in ["Lock", "Cookie", "Socket"] {
        let singleton = session.profile.join(format!("Singleton{name}"));
        remove(&singleton.display().to_string());
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

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .context("cannot set the mode")
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
///
/// The image keeps the shared libraries it links against and dropped the
/// payload, so this is one package: the first boot pays for it, and no rebuild
/// is ever needed for a new release.
async fn install_brave(brave: &Brave) -> Result<()> {
    let installed = brave.binary().is_file();

    if !installed {
        info!(root = %brave.root.display(), "installing the browser");
        unpack_brave().await?;
    } else if brave.upgrade {
        // Ask whether there is anything newer, and take it if so. Off by
        // default because it costs a download on every start.
        info!("checking for a newer browser");
        refresh_index().await?;
        if let Some(newest) = newest_brave_version().await? {
            if Some(newest.as_str()) != Some(current_brave_version().await?.as_str()) {
                unpack_brave().await?;
            }
        }
    } else {
        return Ok(());
    }

    if !brave.binary().is_file() {
        bail!("the browser is not installed at {}", brave.root.display());
    }
    Ok(())
}

/// Fetches the package and hands it to dpkg.
///
/// Not `apt-get install`: with the package recorded as installed and its
/// payload deleted from the image, apt's plan for reinstalling it is not
/// something to rely on. dpkg unpacks what it is given, and its dependencies
/// are already in the image from the build.
async fn unpack_brave() -> Result<()> {
    refresh_index().await?;

    let download = std::env::temp_dir().join("brave");
    let _ = tokio::fs::remove_dir_all(&download).await;
    create(&download, "the download directory").await?;

    command("apt-get", &["download", "brave-browser"], Some(&download)).await?;
    let package = find_package(&download)
        .await
        .with_context(|| "the browser package could not be fetched")?;
    command("dpkg", &["--install", &package.to_string_lossy()], None).await?;

    // The lists are large and only useful for this one call.
    let _ = tokio::fs::remove_dir_all("/var/lib/apt/lists").await;
    let _ = tokio::fs::remove_dir_all(&download).await;
    Ok(())
}

async fn refresh_index() -> Result<()> {
    command(
        "apt-get",
        &["update", "-y", "--no-install-recommends"],
        None,
    )
    .await
    .map(|_| ())
}

/// Whatever apt left in the download directory: it names the file it saved.
async fn find_package(directory: &Path) -> Option<PathBuf> {
    let mut paths = Vec::new();
    let mut entries = tokio::fs::read_dir(directory).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        paths.push(entry.path());
    }
    paths.sort();
    paths.into_iter().find(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("deb"))
    })
}

async fn current_brave_version() -> Result<String> {
    let status = Command::new("dpkg-query")
        .args(["-f", "${Version}", "-W", "brave-browser"])
        .output()
        .await
        .context("cannot ask dpkg what is installed")?;
    let version = String::from_utf8_lossy(&status.stdout).trim().to_owned();
    if version.is_empty() {
        bail!("the image does not record a browser version");
    }
    Ok(version)
}

/// The newest version the repository offers, if it will say.
async fn newest_brave_version() -> Result<Option<String>> {
    let Ok(policy) = command("apt-cache", &["policy", "brave-browser"], None).await else {
        return Ok(None);
    };
    Ok(policy
        .lines()
        .find_map(|line| line.trim().strip_prefix("Candidate: "))
        .map(str::to_owned))
}

/// Runs a command, returning its standard output, and logs the whole exchange
/// so a failed install says why.
async fn command(program: &str, args: &[&str], directory: Option<&Path>) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let output = command
        .output()
        .await
        .with_context(|| format!("cannot run {program}"))?;
    debug!(
        %program,
        args = ?args,
        status = %output.status,
        "{}", String::from_utf8_lossy(&output.stdout)
    );
    if !output.status.success() {
        bail!(
            "{program} {args:?} failed with {}: {}",
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
    let mut command = Command::new(session.brave.root.join("brave").join("brave"));
    command
        .env("DISPLAY", format!(":{}", session.display))
        .arg("--no-sandbox")
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

/// The web service serves until the process goes away, which is what stopping
/// the container means. Nothing shuts it down from inside.
async fn until_the_end() {
    std::future::pending::<()>().await;
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
    let password_file = Path::new("/run/x11vnc/passwd");
    create(Path::new("/run/x11vnc"), "the x11vnc run directory").await?;
    // x11vnc only writes a password file on its own; the server then reads it.
    let status = Command::new("x11vnc")
        .arg("-quiet")
        .arg("-storepasswd")
        .arg(&config.vnc.password)
        .arg(password_file)
        .status()
        .await
        .context("cannot store the VNC password")?;
    if !status.success() {
        bail!("x11vnc could not store the password: {status}");
    }

    let mut command = Command::new("x11vnc");
    command
        .arg("-display")
        .arg(format!(":{}", config.session.display))
        .arg("-rfbport")
        .arg(config.vnc.port.to_string())
        // No -localhost: the compose file already publishes this on the
        // loopback, and a published port is forwarded to the container's own
        // address, which x11vnc would refuse if told to listen on loopback only.
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

/// Binds the two listeners and serves until the stop signal fires.
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
