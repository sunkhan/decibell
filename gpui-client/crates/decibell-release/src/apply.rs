//! Putting a downloaded, verified update in place, with a trial start and a rollback.
//!
//! A download ends staged in the profile (`<profile>/update/<file>` + `staged.json`). It is applied
//! by the **old** version at its next start, before any window (what electron-updater's
//! `autoInstallOnAppQuit` does at quit), or at once by Restart, which starts the old version again
//! with `--apply-update` and quits. Either way the old binary, which is known to work, supervises:
//!
//! 1. it gives up the profile (the next version must become its instance),
//! 2. puts the new version in place, keeping the old one beside it:
//!    - Linux, AppImage: `<name>` → `.<name>.previous`, the staged file → `<name>` (renames in the
//!      AppImage's directory; the running AppImage keeps its mount);
//!    - Windows: runs the staged installer silently (`/S /UPDATE /D=<install dir>`), which moves the
//!      install's files into `<install dir>\previous\` (Windows lets a running image be renamed) and
//!      installs the new ones;
//! 3. starts the new version with `--update-trial=<pipe>` and the start's own arguments, and waits
//!    up to [`TRIAL_TIMEOUT`] for it to write `ok` once its first frame is on screen;
//! 4. `ok`: the old version exits and the backup goes (Linux at once; Windows at the new version's
//!    next start, the old image being mapped until then). No `ok` (it exited, crashed or hung): it is
//!    killed, the old files go back, the version is remembered as failed (never offered again), and
//!    the old version carries on starting (About: "Couldn't check: …").
//!
//! The wait is one blocking read of a pipe on a thread made for it (the pipe closes if the new
//! version dies): no polling.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::version::Version;

/// How long a new version has to show its first frame.
pub const TRIAL_TIMEOUT: Duration = Duration::from_secs(60);
/// The argument a supervised start receives (its value: the pipe to write `ok` to).
pub const TRIAL_ARG: &str = "--update-trial";
/// The argument Restart starts the old version with: wait for the profile, then apply.
pub const APPLY_ARG: &str = "--apply-update";

const STAGED: &str = "staged.json";
const FAILED: &str = "failed.json";

/// A verified download waiting to be applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Staged {
    pub version: String,
    /// The file's name in the update directory.
    pub file: String,
    /// Base64 SHA-512, as the feed gave it.
    pub sha512: String,
}

impl Staged {
    /// The staged update in `dir`, if it is newer than this build.
    pub fn read(dir: &Path) -> Option<Staged> {
        let staged: Staged = serde_json::from_slice(&std::fs::read(dir.join(STAGED)).ok()?).ok()?;
        let newer = Version::parse(&staged.version)? > Version::parse(crate::VERSION)?;
        let plain = !staged.file.is_empty() && !staged.file.contains(['/', '\\']) && !staged.file.starts_with('.');
        (newer && plain).then_some(staged)
    }

    pub fn path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.file)
    }

    /// Size-independent check of the file against its SHA-512 (it was checked when it landed;
    /// this catches anything that touched it since).
    pub fn verify(&self, dir: &Path) -> bool {
        let Ok(expected) = base64_sha512(&self.sha512) else { return false };
        file_sha512(&self.path(dir)).is_ok_and(|got| got == expected)
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        write_atomic(&dir.join(STAGED), &serde_json::to_vec(self).unwrap_or_default())
    }

    /// Removes the staged file and its record.
    pub fn clear(dir: &Path) {
        if let Ok(bytes) = std::fs::read(dir.join(STAGED))
            && let Ok(staged) = serde_json::from_slice::<Staged>(&bytes)
            && !staged.file.contains(['/', '\\'])
        {
            let _ = std::fs::remove_file(dir.join(&staged.file));
        }
        let _ = std::fs::remove_file(dir.join(STAGED));
    }
}

/// A version that didn't start, and why: never offered again; shown once in About.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Failed {
    pub version: String,
    pub reason: String,
    #[serde(default)]
    pub shown: bool,
}

impl Failed {
    pub fn read(dir: &Path) -> Option<Failed> {
        serde_json::from_slice(&std::fs::read(dir.join(FAILED)).ok()?).ok()
    }

    pub fn write(&self, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        let _ = write_atomic(&dir.join(FAILED), &serde_json::to_vec(self).unwrap_or_default());
    }

    /// Whether `version` is the one that failed here.
    pub fn is(dir: &Path, version: &str) -> bool {
        Failed::read(dir).is_some_and(|f| f.version == version)
    }
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The new version is running: this process exits.
    Started,
    /// It didn't start; the old version is back in place and this process carries on.
    RolledBack { version: String, reason: String },
    /// Nothing was changed (why), this process carries on.
    NotApplied(String),
}

/// Applies `staged` (already verified) for this install, the new version started with `args`.
/// Call it holding no lock on the profile.
pub fn apply(dir: &Path, staged: &Staged, args: &[OsString]) -> Outcome {
    let outcome = platform_apply(dir, staged, args, TRIAL_TIMEOUT);
    match &outcome {
        Outcome::Started => Staged::clear(dir),
        Outcome::RolledBack { version, reason } => {
            log::error!("[update] {version} didn't start ({reason}); the old version was put back");
            Failed { version: version.clone(), reason: reason.clone(), shown: false }.write(dir);
            Staged::clear(dir);
        }
        Outcome::NotApplied(why) => log::warn!("[update] not applied: {why}"),
    }
    outcome
}

#[cfg(target_os = "linux")]
fn platform_apply(dir: &Path, staged: &Staged, args: &[OsString], timeout: Duration) -> Outcome {
    let Some(target) = std::env::var_os("APPIMAGE").map(PathBuf::from) else {
        return Outcome::NotApplied("not running from an AppImage".into());
    };
    swap_and_trial(&staged.path(dir), &target, &staged.version, args, timeout)
}

/// Linux: puts `new` at `target` (keeping the old file beside it), starts it on trial, and puts
/// the old file back if the trial fails.
#[cfg(unix)]
pub(crate) fn swap_and_trial(new: &Path, target: &Path, version: &str, args: &[OsString], timeout: Duration) -> Outcome {
    use std::os::unix::fs::PermissionsExt as _;
    let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
        return Outcome::NotApplied(format!("{} is not a file path", target.display()));
    };
    let name = name.to_string_lossy();
    let incoming = parent.join(format!(".{name}.incoming"));
    let previous = parent.join(format!(".{name}.previous"));
    // Copied, not moved: the profile and the AppImage may be on different filesystems.
    let copied = std::fs::copy(new, &incoming)
        .and_then(|_| std::fs::set_permissions(&incoming, std::fs::Permissions::from_mode(0o755)))
        .and_then(|_| File::open(&incoming)?.sync_all());
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&incoming);
        return Outcome::NotApplied(format!("couldn't write beside {}: {e}", target.display()));
    }
    if let Err(e) = std::fs::rename(target, &previous) {
        let _ = std::fs::remove_file(&incoming);
        return Outcome::NotApplied(format!("couldn't move {} aside: {e}", target.display()));
    }
    if let Err(e) = std::fs::rename(&incoming, target) {
        let _ = std::fs::rename(&previous, target);
        let _ = std::fs::remove_file(&incoming);
        return Outcome::NotApplied(format!("couldn't put the update in place: {e}"));
    }
    let mut command = Command::new(target);
    command.args(args);
    match trial(command, timeout) {
        Ok(()) => {
            let _ = std::fs::remove_file(&previous);
            Outcome::Started
        }
        Err(reason) => {
            let _ = std::fs::rename(target, &incoming);
            let _ = std::fs::rename(&previous, target);
            let _ = std::fs::remove_file(&incoming);
            Outcome::RolledBack { version: version.to_string(), reason }
        }
    }
}

#[cfg(windows)]
fn platform_apply(dir: &Path, staged: &Staged, args: &[OsString], timeout: Duration) -> Outcome {
    use std::os::windows::process::CommandExt as _;
    let install = crate::install::current().dir.clone();
    let exe = install.join("decibell.exe");
    // NSIS: `/D=` comes last and unquoted, spaces and all.
    let status = Command::new(staged.path(dir)).args(["/S", "/UPDATE"]).raw_arg(format!("/D={}", install.display())).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            restore_previous(&install);
            return Outcome::RolledBack { version: staged.version.clone(), reason: format!("the installer failed ({status})") };
        }
        Err(e) => return Outcome::NotApplied(format!("couldn't run the installer: {e}")),
    }
    let mut command = Command::new(&exe);
    command.args(args);
    match trial(command, timeout) {
        Ok(()) => Outcome::Started,
        Err(reason) => {
            restore_previous(&install);
            Outcome::RolledBack { version: staged.version.clone(), reason }
        }
    }
}

/// Windows rollback: everything the installer put in `install` moves out, `previous\` moves back.
#[cfg(windows)]
fn restore_previous(install: &Path) {
    let previous = install.join("previous");
    if !previous.is_dir() {
        return;
    }
    let failed = install.join(format!("failed-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&failed);
    if let Ok(entries) = std::fs::read_dir(install) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s == "previous" || s.starts_with("failed-") {
                continue;
            }
            let _ = std::fs::rename(entry.path(), failed.join(&name));
        }
    }
    if let Ok(entries) = std::fs::read_dir(&previous) {
        for entry in entries.flatten() {
            let _ = std::fs::rename(entry.path(), install.join(entry.file_name()));
        }
    }
    let _ = std::fs::remove_dir(&previous);
    let _ = std::fs::remove_dir_all(&failed);
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_apply(_: &Path, _: &Staged, _: &[OsString], _: Duration) -> Outcome {
    Outcome::NotApplied("no self-update on this system".into())
}

/// What a new version leaves after an update: Windows' `previous\` (the old image was mapped by
/// the supervisor until it exited). Best effort, at start.
pub fn clean_up_after_update() {
    #[cfg(windows)]
    {
        let previous = crate::install::current().dir.join("previous");
        if previous.is_dir() {
            match std::fs::remove_dir_all(&previous) {
                Ok(()) => log::info!("[update] removed the previous version"),
                Err(e) => log::info!("[update] the previous version stays for now: {e}"),
            }
        }
    }
}

/// Starts `command` with a trial pipe and waits for `ok` on it.
fn trial(mut command: Command, timeout: Duration) -> Result<(), String> {
    let (reader, mut child) = spawn_with_pipe(&mut command).map_err(|e| format!("it couldn't be started: {e}"))?;
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("decibell-update-trial".into()).spawn(move || {
        let mut said = String::new();
        let _ = reader.take(64).read_to_string(&mut said);
        let _ = tx.send(said);
    });
    if spawned.is_err() {
        kill(&mut child);
        return Err("couldn't wait for it".into());
    }
    match rx.recv_timeout(timeout) {
        Ok(said) if said.starts_with("ok") => {
            log::info!("[update] the new version started");
            Ok(())
        }
        Ok(_) => {
            let status = child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
            Err(format!("it exited before showing a window ({status})"))
        }
        Err(_) => {
            kill(&mut child);
            Err(format!("it showed no window within {} s", timeout.as_secs()))
        }
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn spawn_with_pipe(command: &mut Command) -> std::io::Result<(File, Child)> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded: both descriptors are open and owned by nobody else.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let write_fd = write.as_raw_fd();
    command.arg(format!("{TRIAL_ARG}={write_fd}"));
    // SAFETY: only async-signal-safe calls (fcntl) between fork and exec.
    unsafe {
        command.pre_exec(move || {
            // The child keeps the write end across exec; nothing else of ours does.
            if libc::fcntl(write_fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // A file just written may still be open for writing in a child another thread forked
    // meanwhile (until that child execs): exec then fails with ETXTBSY for a moment.
    let mut tries = 0;
    let child = loop {
        match command.spawn() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            other => break other?,
        }
    };
    // Our copy goes, so the pipe ends when the child does.
    drop(write);
    Ok((File::from(read), child))
}

#[cfg(windows)]
fn spawn_with_pipe(command: &mut Command) -> std::io::Result<(File, Child)> {
    use std::os::windows::io::FromRawHandle as _;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, TRUE};
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Pipes::CreatePipe;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: the out-pointers are valid; the attributes live across the call.
    unsafe { CreatePipe(&mut read, &mut write, Some(&attributes), 0) }.map_err(std::io::Error::other)?;
    // Only the write end is inherited (std's CreateProcessW inherits inheritable handles).
    // SAFETY: `read` is the open handle CreatePipe returned.
    if let Err(e) = unsafe { SetHandleInformation(read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) } {
        // SAFETY: both handles are open and ours.
        unsafe {
            let _ = CloseHandle(read);
            let _ = CloseHandle(write);
        }
        return Err(std::io::Error::other(e));
    }
    command.arg(format!("{TRIAL_ARG}={}", write.0 as usize));
    let spawned = command.spawn();
    // SAFETY: `write` is open and ours; the child has its own copy.
    unsafe {
        let _ = CloseHandle(write);
    }
    // SAFETY: `read` is an open handle nothing else owns.
    let reader = unsafe { File::from_raw_handle(read.0) };
    Ok((reader, spawned?))
}

/// The new version's side: its first frame is up, so tell the supervisor (once).
pub fn report_started(value: &str) {
    let Ok(raw) = value.parse::<usize>() else { return };
    match trial_pipe(raw) {
        Some(mut pipe) => {
            let _ = pipe.write_all(b"ok\n");
            log::info!("[update] told the previous version that this one started");
        }
        None => log::warn!("[update] {TRIAL_ARG}={value} is not a pipe"),
    }
}

#[cfg(unix)]
fn trial_pipe(raw: usize) -> Option<File> {
    use std::os::fd::FromRawFd as _;
    let fd = i32::try_from(raw).ok().filter(|fd| *fd > 2)?;
    // SAFETY: `stat` is plain data the call fills.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat on any integer is safe; it fails on a closed descriptor.
    if unsafe { libc::fstat(fd, &mut stat) } != 0 || stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return None;
    }
    // SAFETY: the descriptor is an open pipe this process inherited for exactly this, used once.
    Some(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn trial_pipe(raw: usize) -> Option<File> {
    use std::os::windows::io::FromRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
    let handle = HANDLE(raw as *mut std::ffi::c_void);
    // SAFETY: GetFileType on any handle value is safe; an invalid one answers unknown.
    if unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
        return None;
    }
    // SAFETY: the handle is an open pipe this process inherited for exactly this, used once.
    Some(unsafe { File::from_raw_handle(handle.0) })
}

#[cfg(not(any(unix, windows)))]
fn trial_pipe(_: usize) -> Option<File> {
    None
}

pub(crate) fn base64_sha512(b64: &str) -> Result<[u8; 64], ()> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok().and_then(|b| b.try_into().ok()).ok_or(())
}

pub(crate) fn file_sha512(path: &Path) -> std::io::Result<[u8; 64]> {
    use sha2::Digest as _;
    let mut file = File::open(path)?;
    let mut hasher = sha2::Sha512::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Writes through a temporary file and a rename, so a reader never sees half a file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A stand-in "AppImage": a shell script that does what `body` says with the trial argument.
    fn script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The trial fd is the value of `--update-trial=<fd>` (the script's last argument).
    const SAYS_OK: &str = r#"for a in "$@"; do case "$a" in --update-trial=*) fd="${a#--update-trial=}";; esac; done; echo "$@" > "$(dirname "$0")/args"; eval "echo ok >&$fd""#;

    fn setup(old: &str, new: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Decibell.AppImage");
        let staged = dir.path().join("staged-new");
        script(&target, old);
        script(&staged, new);
        (dir, target, staged)
    }

    #[test]
    fn a_new_version_that_starts_replaces_the_old() {
        let (dir, target, staged) = setup("# old", SAYS_OK);
        let args = [OsString::from("decibell://invite/ABC")];
        let outcome = swap_and_trial(&staged, &target, "9.9.9", &args, Duration::from_secs(10));
        assert_eq!(outcome, Outcome::Started);
        assert!(std::fs::read_to_string(&target).unwrap().contains("echo ok"), "the new file is in place");
        assert!(!dir.path().join(".Decibell.AppImage.previous").exists(), "the backup went");
        // The start's own arguments reached the new version.
        std::thread::sleep(Duration::from_millis(100));
        assert!(std::fs::read_to_string(dir.path().join("args")).unwrap().contains("decibell://invite/ABC"));
    }

    #[test]
    fn a_new_version_that_exits_is_rolled_back() {
        let (dir, target, staged) = setup("# old", "exit 3");
        let outcome = swap_and_trial(&staged, &target, "9.9.9", &[], Duration::from_secs(10));
        let Outcome::RolledBack { reason, .. } = outcome else { panic!("{outcome:?}") };
        assert!(reason.contains("exited before showing a window"), "{reason}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "#!/bin/sh\n# old\n", "the old file is back");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "no leftovers: {names:?}");
    }

    #[test]
    fn a_new_version_that_hangs_is_killed_and_rolled_back() {
        let (_dir, target, staged) = setup("# old", "sleep 30");
        let started = std::time::Instant::now();
        let outcome = swap_and_trial(&staged, &target, "9.9.9", &[], Duration::from_millis(500));
        assert!(matches!(outcome, Outcome::RolledBack { ref reason, .. } if reason.contains("no window")), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "the hung child was killed, not waited for");
        assert!(std::fs::read_to_string(&target).unwrap().contains("# old"));
    }

    #[test]
    fn staged_and_failed_records() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Decibell-99.0.0-x86_64.AppImage");
        std::fs::write(&file, b"payload").unwrap();
        use base64::Engine as _;
        let sha = base64::engine::general_purpose::STANDARD.encode(file_sha512(&file).unwrap());
        let staged = Staged { version: "99.0.0".into(), file: "Decibell-99.0.0-x86_64.AppImage".into(), sha512: sha };
        staged.write(dir.path()).unwrap();
        assert_eq!(Staged::read(dir.path()), Some(staged.clone()));
        assert!(staged.verify(dir.path()));
        std::fs::write(&file, b"tampered").unwrap();
        assert!(!staged.verify(dir.path()), "a file changed after it landed");
        // An older or equal version, or a path, is not an update.
        for (version, name) in [("0.0.1", "x"), (crate::VERSION, "x"), ("99.0.0", "../x"), ("99.0.0", ".hidden")] {
            Staged { version: version.into(), file: name.into(), sha512: String::new() }.write(dir.path()).unwrap();
            assert_eq!(Staged::read(dir.path()), None, "{version} {name}");
        }
        Staged::clear(dir.path());
        assert!(!dir.path().join(STAGED).exists());

        Failed { version: "99.0.0".into(), reason: "r".into(), shown: false }.write(dir.path());
        assert!(Failed::is(dir.path(), "99.0.0"));
        assert!(!Failed::is(dir.path(), "99.0.1"));
    }

    #[test]
    fn a_trial_argument_that_is_not_a_pipe_is_ignored() {
        // stdout of the test runner (fd 1 is refused outright), a closed descriptor, garbage.
        report_started("1");
        report_started("987");
        report_started("x");
        let file = tempfile::tempfile().unwrap();
        use std::os::fd::AsRawFd as _;
        report_started(&file.as_raw_fd().to_string());
        assert_eq!(file.metadata().unwrap().len(), 0, "a regular file is not a trial pipe");
    }
}
