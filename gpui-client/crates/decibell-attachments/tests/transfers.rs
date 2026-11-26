//! Upload / download / manager / media state machines against the mock attachment server
//! (tests/support), including the server's refusals and network faults.

mod support;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use decibell_attachments::crypto::{ciphertext_size, CHUNK_BYTES};
use decibell_attachments::manager::{DownloadContext, DownloadEvent, DownloadManager, DownloadState, ManagerOptions, StartArgs};
use decibell_attachments::manager::Destination;
use decibell_attachments::media::{ImageCache, ImageKey, ImageVariant, MediaReader};
use decibell_attachments::upload::{prepare, UploadControl, UploadProgress, UploadSource};
use decibell_attachments::{AttachmentKey, AttachmentKind, Attachments, Error, FileKey};
use support::*;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("dba-{tag}-{}-{}", std::process::id(), rand_u32()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    getrandom(&mut b);
    u32::from_le_bytes(b)
}

fn getrandom(b: &mut [u8]) {
    // A cheap unique-ish value without another dev-dependency.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    for (i, x) in b.iter_mut().enumerate() {
        *x = (t >> (i * 8)) as u8;
    }
}

async fn setup() -> (Arc<Mock>, Arc<Attachments>) {
    let mock = Mock::start().await;
    let svc = service();
    svc.set_target("srv", mock.target());
    (mock, svc)
}

fn write_file(dir: &std::path::Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 3) as u8, (y * 5) as u8, 77, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

async fn upload_path(svc: &Arc<Attachments>, path: PathBuf, encrypted: bool, ctl: &UploadControl) -> (Result<decibell_attachments::upload::UploadedAttachment, Error>, Vec<UploadProgress>) {
    let prepared = prepare(UploadSource::Path(path), None).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let r = svc.upload("srv", "general", &prepared, encrypted, ctl, move |p| s2.lock().unwrap().push(p)).await;
    let v = seen.lock().unwrap().clone();
    (r, v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plain_upload_with_thumbnails_and_reuse() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-plain");
    let pic = png(1500, 900);
    let path = write_file(&dir, "Shot.png", &pic);
    let prepared = prepare(UploadSource::Path(path.clone()), None).await.unwrap();
    assert_eq!((prepared.kind, prepared.width, prepared.height), (AttachmentKind::Image, 1500, 900));
    assert_eq!(prepared.mime, "image/png");
    assert_eq!(prepared.thumbnails.iter().map(|t| t.0).collect::<Vec<_>>(), vec![320, 640, 1280]);
    assert!(!prepared.placeholder.is_empty());
    let ctl = UploadControl::new();
    let r = svc.upload("srv", "general", &prepared, false, &ctl, |_| {}).await.unwrap();
    let f = mock.file(r.id).unwrap();
    assert!(f.ready && !f.encrypted);
    assert_eq!(f.data, pic);
    assert_eq!((f.width, f.height, f.filename.as_str()), (1500, 900, "Shot.png"));
    assert_eq!(f.placeholder, prepared.placeholder);
    assert_eq!(r.thumbnail_sizes_mask, 7);
    assert_eq!(f.thumbs.len(), 3);
    assert!(r.encrypted.is_none());
    // One TLS connection carried init, PATCH, complete and three thumbnails.
    assert_eq!(mock.connections.load(Ordering::SeqCst), 1, "{:?}", mock.requests.lock().unwrap());
    // Read back whole and by range over the same pool.
    let all = svc.fetch_all("srv", r.id, None).await.unwrap();
    assert_eq!(all, pic);
    let mut s = svc.open("srv", r.id, 1000, Some(1999), None).await.unwrap();
    let mut part = Vec::new();
    s.read_to_end(&mut part).await.unwrap();
    assert_eq!(part, &pic[1000..2000]);
    assert_eq!(mock.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sealed_upload_roundtrip() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-sealed");
    let data = plaintext(3 * 1024 * 1024 + 12345);
    let path = write_file(&dir, "notes.bin", &data);
    let (r, prog) = upload_path(&svc, path, true, &UploadControl::new()).await;
    let r = r.unwrap();
    let f = mock.file(r.id).unwrap();
    // The server holds ciphertext under stand-in metadata.
    assert!(f.encrypted && f.ready);
    assert_eq!((f.filename.as_str(), f.mime.as_str(), f.kind), ("encrypted", "application/octet-stream", 2));
    assert_eq!(f.data.len() as u64, ciphertext_size(data.len() as u64, CHUNK_BYTES));
    let meta = r.encrypted.clone().unwrap();
    assert_eq!((meta.filename.as_str(), meta.size_bytes, meta.chunk_bytes), ("notes.bin", data.len() as i64, 65536));
    let key = FileKey::from_b64(&meta.key_b64).unwrap();
    assert_eq!(key.open_all(&f.data, data.len() as u64, CHUNK_BYTES).unwrap(), data);
    // Progress: bounded rate, ends at the total.
    assert_eq!(prog.last().unwrap().sent, data.len() as u64);
    assert!(prog.len() < 50, "{} progress calls", prog.len());
    // Our own key was registered: the reader opens it at once.
    assert_eq!(svc.fetch_all("srv", r.id, None).await.unwrap(), data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sealed_thumbnails_and_fallback_size() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-thumb");
    let path = write_file(&dir, "p.png", &png(800, 600));
    let (r, _) = upload_path(&svc, path, true, &UploadControl::new()).await;
    let r = r.unwrap();
    let meta = r.encrypted.unwrap();
    assert_eq!(meta.thumbnail_sizes_mask, 7);
    assert_eq!((meta.width, meta.height, meta.mime.as_str()), (800, 600, "image/png"));
    let f = mock.file(r.id).unwrap();
    assert_eq!((f.width, f.height, f.placeholder.as_str()), (0, 0, ""), "sealed init leaks nothing");
    let key = FileKey::from_b64(&meta.key_b64).unwrap();
    for (size, sealed) in &f.thumbs {
        let jpeg = key.open_thumbnail(*size, sealed).unwrap();
        assert_eq!(&jpeg[..2], b"\xff\xd8");
    }
    let jpeg = svc.fetch_thumbnail("srv", r.id, 640, None).await.unwrap();
    assert_eq!(&jpeg[..2], b"\xff\xd8");
    // Only a 1280 on the server: asking 640 is served 1280, which opens under 1280's nonce.
    mock.files.lock().unwrap().get_mut(&r.id).unwrap().thumbs.retain(|s, _| *s == 1280);
    let jpeg = svc.fetch_thumbnail("srv", r.id, 640, None).await.unwrap();
    let d = image::load_from_memory(&jpeg).unwrap();
    assert_eq!(d.width(), 800, "the 1280 thumbnail of an 800 px picture is 800 px");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumes_from_the_servers_offset_after_a_cut() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-cut");
    for encrypted in [false, true] {
        let data = plaintext(2 * 1024 * 1024 + 777);
        let path = write_file(&dir, "big.bin", &data);
        // The connection drops 100,000 bytes into the first PATCH (mid sealed-chunk when sealed);
        // the server keeps those bytes, so the client must HEAD and continue from there.
        *mock.knobs.cut_next_patch_after.lock().unwrap() = Some(100_000);
        let heads_before = mock.count("HEAD");
        let (r, _) = upload_path(&svc, path, encrypted, &UploadControl::new()).await;
        let r = r.unwrap();
        let f = mock.file(r.id).unwrap();
        let plain = match &r.encrypted {
            Some(m) => FileKey::from_b64(&m.key_b64).unwrap().open_all(&f.data, data.len() as u64, CHUNK_BYTES).unwrap(),
            None => f.data.clone(),
        };
        assert_eq!(plain, data, "encrypted={encrypted}");
        assert_eq!(mock.count("HEAD") - heads_before, 1, "one realignment");
    }
    // A 409 (offset disagreement) is realigned the same way.
    *mock.knobs.patch_status.lock().unwrap() = Some((409, 1));
    let data = plaintext(300_000);
    let (r, _) = upload_path(&svc, write_file(&dir, "c.bin", &data), false, &UploadControl::new()).await;
    assert_eq!(mock.file(r.unwrap().id).unwrap().data, data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_refusals_map_to_errors() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-refuse");
    let path = write_file(&dir, "a.bin", &plaintext(200_000));

    // The target's cap: refused before anything is sent.
    let mut t = mock.target();
    t.max_bytes = 100_000;
    svc.set_target("srv", t);
    let before = mock.count("POST /attachments/init");
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::TooLarge)), "{r:?}");
    assert_eq!(mock.count("POST /attachments/init"), before);
    // Sealed counts the ciphertext against the cap.
    let mut t = mock.target();
    t.max_bytes = 200_000;
    svc.set_target("srv", t);
    let (r, _) = upload_path(&svc, path.clone(), true, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::TooLarge)), "{r:?}");
    svc.set_target("srv", mock.target());

    // The server's own cap (413) and the free-space headroom (507).
    mock.knobs.max_bytes.store(1000, Ordering::SeqCst);
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::TooLarge)), "{r:?}");
    mock.knobs.max_bytes.store(0, Ordering::SeqCst);
    mock.knobs.free_bytes.store(1000, Ordering::SeqCst);
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::InsufficientStorage)), "{r:?}");
    assert_eq!(r.unwrap_err().status(), Some(507));
    mock.knobs.free_bytes.store(u64::MAX, Ordering::SeqCst);

    // 403 at init: no retry.
    *mock.knobs.init_refuse.lock().unwrap() = Some((403, 1));
    let before = mock.count("POST /attachments/init");
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::Http(403))), "{r:?}");
    assert_eq!(mock.count("POST /attachments/init") - before, 1);

    // 429 (the init bucket) waits and tries again.
    *mock.knobs.init_refuse.lock().unwrap() = Some((429, 1));
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(r.is_ok(), "{r:?}");

    // Swept under us (404 on PATCH): no retry, whether the 404 or the closed connection comes
    // first (the HEAD settles it).
    mock.knobs.sweep_next_patch.store(true, Ordering::SeqCst);
    let before = mock.count("PATCH");
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::UploadExpired)), "{r:?}");
    assert_eq!(mock.count("PATCH") - before, 1);
    assert_eq!(r.unwrap_err().to_string(), "Upload expired on the server");

    // 500 on every PATCH: 1 + 5 retries, then the error.
    *mock.knobs.patch_status.lock().unwrap() = Some((500, 6));
    let before = mock.count("PATCH");
    let t0 = Instant::now();
    let (r, _) = upload_path(&svc, path.clone(), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::Http(500))), "{r:?}");
    assert_eq!(mock.count("PATCH") - before, 6);
    assert!(t0.elapsed() >= Duration::from_millis(500 + 1000 + 2000 + 4000 + 8000 - 100), "backoff {:?}", t0.elapsed());

    // An empty file never reaches the server (it refuses size 0).
    let (r, _) = upload_path(&svc, write_file(&dir, "empty", b""), false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::Invalid(_))));

    // Not connected.
    svc.clear_target("srv");
    let (r, _) = upload_path(&svc, path, false, &UploadControl::new()).await;
    assert!(matches!(r, Err(Error::NotConnected)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pinned_to_the_fingerprint() {
    let (mock, svc) = setup().await;
    let id = mock.seed(b"hello".to_vec(), false);
    let mut t = mock.target();
    t.cert_fingerprint = "00".repeat(32);
    svc.set_target("srv", t);
    match svc.fetch_all("srv", id, None).await {
        Err(Error::CertMismatch(m)) => assert!(m.contains(&test_cert_fingerprint()), "{m}"),
        other => panic!("{other:?}"),
    }
    let mut t = mock.target();
    t.cert_fingerprint = String::new();
    svc.set_target("srv", t);
    assert!(matches!(svc.fetch_all("srv", id, None).await, Err(Error::CertMismatch(_))));
    svc.set_target("srv", mock.target());
    assert_eq!(svc.fetch_all("srv", id, None).await.unwrap(), b"hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_and_pause() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-ctl");
    let data = plaintext(4 * 1024 * 1024);
    let path = write_file(&dir, "c.bin", &data);
    svc.limits().set_upload(1024 * 1024); // slow enough to act on

    // Cancel mid-upload: the error, and the server row is abandoned (DELETE).
    let ctl = UploadControl::new();
    let c2 = ctl.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        c2.cancel();
    });
    let t0 = Instant::now();
    let (r, _) = upload_path(&svc, path.clone(), false, &ctl).await;
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(t0.elapsed() < Duration::from_millis(1200), "cancel bites at once: {:?}", t0.elapsed());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.count("DELETE"), 1);
    assert!(mock.files.lock().unwrap().is_empty());

    // Pause before the next PATCH; nothing is sent while paused; resume finishes.
    let ctl = UploadControl::new();
    let prepared = prepare(UploadSource::Path(path.clone()), None).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::<UploadProgress>::new()));
    let s2 = seen.clone();
    let svc2 = svc.clone();
    let c2 = ctl.clone();
    let task = tokio::spawn(async move { svc2.upload("srv", "general", &prepared, false, &c2, move |p| s2.lock().unwrap().push(p)).await });
    tokio::time::sleep(Duration::from_millis(600)).await;
    ctl.pause();
    tokio::time::sleep(Duration::from_millis(800)).await; // the PATCH in flight finishes
    let patches = mock.count("PATCH");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(mock.count("PATCH"), patches, "no PATCH while paused");
    assert!(seen.lock().unwrap().iter().any(|p| p.paused), "paused was reported");
    ctl.resume();
    let r = task.await.unwrap().unwrap();
    assert_eq!(mock.file(r.id).unwrap().data, data);
    svc.limits().set_upload(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_speed_cap_holds() {
    let (mock, svc) = setup().await;
    let dir = tmpdir("up-cap");
    let data = plaintext(1536 * 1024);
    let path = write_file(&dir, "cap.bin", &data);
    svc.limits().set_upload(512 * 1024);
    let t0 = Instant::now();
    let (r, _) = upload_path(&svc, path, false, &UploadControl::new()).await;
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(mock.file(r.unwrap().id).unwrap().data.len(), data.len());
    let rate = data.len() as f64 / secs / 1024.0;
    println!("upload of 1.5 MiB capped at 512 KiB/s: {secs:.2} s = {rate:.0} KiB/s");
    assert!((420.0..=560.0).contains(&rate), "rate {rate:.0} KiB/s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn download_cap_and_media_reader() {
    let (mock, svc) = setup().await;
    let data = plaintext(1024 * 1024);
    let id = mock.seed(data.clone(), false);
    svc.limits().set_download(300 * 1024);
    let t0 = Instant::now();
    let got = svc.fetch_all("srv", id, None).await.unwrap();
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(got, data);
    let rate = data.len() as f64 / secs / 1024.0;
    println!("download of 1 MiB capped at 300 KiB/s: {secs:.2} s = {rate:.0} KiB/s");
    assert!((250.0..=330.0).contains(&rate), "rate {rate:.0} KiB/s");
    svc.limits().set_download(0);

    // Seekable reads, plain and sealed, through the read-ahead pump.
    let key = FileKey::generate();
    let sealed_id = mock.seed(key.seal_all(&data), true);
    svc.register_key("srv", sealed_id, AttachmentKey { key, chunk_bytes: CHUNK_BYTES, size_bytes: data.len() as u64, mime: "video/mp4".into(), filename: "v.mp4".into() });
    for id in [id, sealed_id] {
        let mut r = MediaReader::new(svc.clone(), "srv", id);
        assert_eq!(r.probe_size().await.unwrap(), data.len() as u64);
        let mut buf = vec![0u8; 50_000];
        let mut whole = Vec::new();
        loop {
            let n = r.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            whole.extend_from_slice(&buf[..n]);
        }
        assert_eq!(whole, data);
        for &(pos, len) in &[(65535u64, 3usize), (700_000, 100_000), (10, 10), (1_048_570, 100), (65536 * 3, 65536)] {
            r.seek(pos);
            let mut out = vec![0u8; len];
            let mut got = 0;
            while got < len {
                let n = r.read(&mut out[got..]).await.unwrap();
                if n == 0 {
                    break;
                }
                got += n;
            }
            let end = (pos as usize + len).min(data.len());
            assert_eq!(&out[..got], &data[pos as usize..end], "id {id} at {pos}");
        }
        // The blocking view (a decoder thread).
        let rd = MediaReader::new(svc.clone(), "srv", id).blocking();
        let back = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let mut rd = rd;
            rd.seek(SeekFrom::End(-1000)).unwrap();
            let mut tail = Vec::new();
            rd.read_to_end(&mut tail).unwrap();
            tail
        })
        .await
        .unwrap();
        assert_eq!(back, &data[data.len() - 1000..]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn image_cache_shares_and_bounds() {
    let (mock, svc) = setup().await;
    let a = mock.seed(vec![1u8; 40_000], false);
    let b = mock.seed(vec![2u8; 40_000], false);
    let cache = Arc::new(ImageCache::new(70_000));
    let key = |id| ImageKey { server_id: "srv".into(), attachment_id: id, variant: ImageVariant::Original };
    let (c1, c2, s1, s2) = (cache.clone(), cache.clone(), svc.clone(), svc.clone());
    let (r1, r2) = tokio::join!(c1.get_or_fetch(&s1, key(a)), c2.get_or_fetch(&s2, key(a)));
    assert_eq!(r1.unwrap(), r2.unwrap());
    assert_eq!(mock.count(&format!("GET /attachments/{a}")), 1, "two askers, one fetch");
    cache.get_or_fetch(&svc, key(b)).await.unwrap();
    assert_eq!(cache.len(), 1, "budget 70 KB holds one 40 KB picture");
    assert!(cache.get(&key(a)).is_none() && cache.get(&key(b)).is_some());
}

fn manager(svc: &Arc<Attachments>, state: &std::path::Path, dl: &std::path::Path) -> (DownloadManager, Arc<Mutex<Vec<DownloadEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    let m = DownloadManager::new(
        svc.clone(),
        ManagerOptions { state_dir: state.to_path_buf(), default_dir: dl.to_path_buf(), key_wrap: None },
        move |e| e2.lock().unwrap().push(e),
    );
    (m, events)
}

async fn wait_state(m: &DownloadManager, id: &str, want: DownloadState) -> decibell_attachments::manager::DownloadView {
    for _ in 0..600 {
        if let Some(v) = m.list().into_iter().find(|v| v.id == id) {
            if v.state == want {
                return v;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{id} never reached {want:?}: {:?}", m.list());
}

fn args(id: i64, name: &str, size: u64) -> StartArgs {
    StartArgs {
        server_id: "srv".into(),
        attachment_id: id,
        filename: name.into(),
        mime: "application/octet-stream".into(),
        kind: AttachmentKind::Document,
        size_bytes: size,
        context: DownloadContext { server_name: "Mock".into(), ..Default::default() },
        destination: Destination::Folder,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_downloads_pauses_resumes_and_persists() {
    let (mock, svc) = setup().await;
    let state = tmpdir("mgr-state");
    let dl = tmpdir("mgr-dl");
    let data = plaintext(1536 * 1024);
    let id = mock.seed(data.clone(), false);
    let (m, events) = manager(&svc, &state, &dl);

    // Straight to the folder, under a sanitised name.
    let d1 = m.start(args(id, "../evil:name.bin", data.len() as u64)).await.unwrap();
    let v = wait_state(&m, &d1, DownloadState::Done).await;
    assert_eq!(v.filename, "_evil_name.bin");
    assert_eq!(std::fs::read(dl.join("_evil_name.bin")).unwrap(), data);
    // Same name again: a free one.
    let d2 = m.start(args(id, "../evil:name.bin", data.len() as u64)).await.unwrap();
    let v = wait_state(&m, &d2, DownloadState::Done).await;
    assert_eq!(v.filename, "_evil_name (1).bin");

    // Pause mid-way (capped so there is a mid-way), resume over Range.
    svc.limits().set_download(512 * 1024);
    let d3 = m.start(args(id, "paused.bin", data.len() as u64)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    m.pause(&d3);
    let v = wait_state(&m, &d3, DownloadState::Paused).await;
    let part = dl.join("paused.bin.part");
    let have = std::fs::metadata(&part).unwrap().len();
    assert!(have > 0 && have < data.len() as u64, "part {have}");
    assert_eq!(v.received_bytes, have);
    svc.limits().set_download(0);
    let ranged_before = mock.requests.lock().unwrap().len();
    m.resume(&d3);
    wait_state(&m, &d3, DownloadState::Done).await;
    assert_eq!(std::fs::read(dl.join("paused.bin")).unwrap(), data);
    assert!(!part.exists());
    assert!(mock.requests.lock().unwrap().len() > ranged_before);

    // Cancel deletes the part file.
    svc.limits().set_download(200 * 1024);
    let d4 = m.start(args(id, "cancel.bin", data.len() as u64)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    m.cancel(&d4);
    let v = wait_state(&m, &d4, DownloadState::Cancelled).await;
    assert_eq!(v.received_bytes, 0);
    assert!(!dl.join("cancel.bin.part").exists());

    // Quit mid-download: paused on disk; a new manager lists it paused at the right count.
    let d5 = m.start(args(id, "quit.bin", data.len() as u64)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    m.pause_all_for_quit();
    tokio::time::sleep(Duration::from_millis(200)).await;
    svc.limits().set_download(0);
    let (m2, _) = manager(&svc, &state, &dl);
    let v = m2.list().into_iter().find(|v| v.id == d5).unwrap();
    assert_eq!(v.state, DownloadState::Paused);
    assert_eq!(v.received_bytes, std::fs::metadata(dl.join("quit.bin.part")).unwrap().len());
    m2.resume(&d5);
    wait_state(&m2, &d5, DownloadState::Done).await;
    assert_eq!(std::fs::read(dl.join("quit.bin")).unwrap(), data);

    // A deleted file shows as missing; clear removes finished rows but keeps files.
    std::fs::remove_file(dl.join("_evil_name.bin")).unwrap();
    assert!(m2.list().iter().find(|v| v.id == d1).unwrap().missing);
    m2.clear_finished();
    assert!(m2.list().is_empty());
    assert!(dl.join("quit.bin").exists());

    // Events: progress ticks and state changes; never a key.
    let ev = events.lock().unwrap();
    assert!(ev.iter().any(|e| matches!(e, DownloadEvent::Changed(v) if v.state == DownloadState::Active && v.received_bytes > 0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manager_sealed_failures_and_odd_servers() {
    let (mock, svc) = setup().await;
    let state = tmpdir("mgr2-state");
    let dl = tmpdir("mgr2-dl");
    let data = plaintext(700_001);
    let key = FileKey::generate();
    let sid = mock.seed(key.seal_all(&data), true);
    let akey = AttachmentKey { key, chunk_bytes: CHUNK_BYTES, size_bytes: data.len() as u64, mime: "x/y".into(), filename: "s".into() };
    svc.register_key("srv", sid, akey.clone());
    let (m, _) = manager(&svc, &state, &dl);

    // Sealed: decrypted to disk; paused mid-chunk and resumed after the registry was cleared
    // (the manager kept its own copy of the key).
    svc.limits().set_download(256 * 1024);
    let d = m.start(args(sid, "sealed.bin", 0)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    m.pause(&d);
    wait_state(&m, &d, DownloadState::Paused).await;
    svc.clear_keys(Some("srv"));
    svc.limits().set_download(0);
    m.resume(&d);
    let v = wait_state(&m, &d, DownloadState::Done).await;
    assert!(v.encrypted);
    assert_eq!(std::fs::read(dl.join("sealed.bin")).unwrap(), data);

    // Paused at quit, restarted without a keychain and without the message opened: the key is
    // gone, and the row says what to do. Once the message is opened again, it resumes.
    svc.register_key("srv", sid, akey.clone());
    svc.limits().set_download(200 * 1024);
    let d = m.start(args(sid, "nokey.bin", 0)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    m.pause_all_for_quit();
    tokio::time::sleep(Duration::from_millis(200)).await;
    svc.limits().set_download(0);
    svc.clear_keys(None);
    let (m2, _) = manager(&svc, &state, &dl);
    m2.resume(&d);
    let v = wait_state(&m2, &d, DownloadState::Failed).await;
    assert_eq!(v.error.as_deref(), Some("Open the file's channel again, then retry"));
    assert!(v.received_bytes > 0, "the part file is kept");
    svc.register_key("srv", sid, akey);
    m2.resume(&d);
    wait_state(&m2, &d, DownloadState::Done).await;
    assert_eq!(std::fs::read(dl.join("nokey.bin")).unwrap(), data);

    // A server that ignores Range on resume (200): still right.
    let pid = mock.seed(data.clone(), false);
    svc.limits().set_download(256 * 1024);
    let d = m.start(args(pid, "norange.bin", data.len() as u64)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    m.pause(&d);
    wait_state(&m, &d, DownloadState::Paused).await;
    svc.limits().set_download(0);
    mock.knobs.ignore_range.store(true, Ordering::SeqCst);
    m.resume(&d);
    wait_state(&m, &d, DownloadState::Done).await;
    mock.knobs.ignore_range.store(false, Ordering::SeqCst);
    assert_eq!(std::fs::read(dl.join("norange.bin")).unwrap(), data);

    // A connection cut mid-body: failed, the part kept; retry finishes.
    *mock.knobs.cut_next_get_after.lock().unwrap() = Some(300_000);
    let d = m.start(args(pid, "cut.bin", data.len() as u64)).await.unwrap();
    let v = wait_state(&m, &d, DownloadState::Failed).await;
    assert_eq!(v.error.as_deref(), Some("Network error"));
    assert!(std::fs::metadata(dl.join("cut.bin.part")).unwrap().len() > 0);
    m.resume(&d);
    wait_state(&m, &d, DownloadState::Done).await;
    assert_eq!(std::fs::read(dl.join("cut.bin")).unwrap(), data);

    // Gone from the server.
    let d = m.start(args(99_999, "gone.bin", 10)).await.unwrap();
    let v = wait_state(&m, &d, DownloadState::Failed).await;
    assert_eq!(v.error.as_deref(), Some("No longer on the server"));

    // A chosen path (save dialog) that is already being downloaded to.
    svc.limits().set_download(100 * 1024);
    let chosen = dl.join("chosen.bin");
    let mut a = args(pid, "x", data.len() as u64);
    a.destination = Destination::Chosen(chosen.clone());
    let d = m.start(a.clone()).await.unwrap();
    let again = m.start(a).await;
    assert_eq!(again.unwrap_err().to_string(), "That file is already being downloaded");
    svc.limits().set_download(0);
    m.remove(std::slice::from_ref(&d));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!m.list().iter().any(|v| v.id == d));
    assert!(!dl.join("chosen.bin.part").exists());

    // Not connected.
    svc.clear_target("srv");
    let e = m.start(args(pid, "nc.bin", 1)).await.unwrap_err();
    assert_eq!(e.to_string(), "Not connected to Mock");
}
