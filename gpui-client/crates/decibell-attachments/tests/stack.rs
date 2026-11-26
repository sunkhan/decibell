//! Against the local test stack (`tools/dev/local-stack.sh up`; loopback only): set
//! `DECIBELL_STACK=1`. As alice, into Daily Gaming: a plain file and a picture with thumbnails
//! uploaded through this crate and bound to a real `#general` message (the CHANNEL_MSG goes out
//! through tests/stack_helper.py, which signs in with tools/dev/devlib.py: this crate has no
//! TCP protocol); both read back whole, by range, through the downloads manager paused and
//! resumed, with a speed cap (rate measured); a sealed file uploaded to `#vault` (ciphertext,
//! stand-in metadata) and read back decrypted, whole and by range. The sealed upload is not
//! bound to a message: that needs the channel's epoch key (core's E2EE), not this crate.

mod support;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_attachments::crypto::{ciphertext_size, CHUNK_BYTES};
use decibell_attachments::manager::{Destination, DownloadContext, DownloadManager, DownloadState, ManagerOptions, StartArgs};
use decibell_attachments::upload::{prepare, UploadControl, UploadSource};
use decibell_attachments::{AttachmentKind, AttachmentTarget, Attachments};
use serde_json::Value;
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Helper {
    child: tokio::process::Child,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl Helper {
    async fn start() -> (Helper, Value) {
        let dev = std::env::var("DECIBELL_DEV_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".cache/decibell-dev"));
        let py = dev.join("deps/central/pgvenv/bin/python");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/stack_helper.py");
        let mut child = tokio::process::Command::new(py)
            .arg("-I")
            .arg(script)
            .arg(repo_root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the stack's venv python");
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let first = tokio::time::timeout(Duration::from_secs(60), lines.next_line()).await.unwrap().unwrap().expect("helper output");
        let v: Value = serde_json::from_str(&first).unwrap();
        assert!(v.get("error").is_none(), "helper: {v}");
        (Helper { child, lines }, v)
    }

    async fn send(&mut self, channel: &str, content: &str, ids: &[i64]) -> Value {
        let req = serde_json::json!({"cmd": "send", "channel": channel, "content": content, "attachments": ids});
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        stdin.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(30), self.lines.next_line()).await.unwrap().unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn delete(&mut self, channel: &str, id: i64) -> Value {
        let req = serde_json::json!({"cmd": "delete", "channel": channel, "id": id});
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(format!("{req}\n").as_bytes()).await.unwrap();
        stdin.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(30), self.lines.next_line()).await.unwrap().unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn quit(mut self) {
        let stdin = self.child.stdin.as_mut().unwrap();
        let _ = stdin.write_all(b"{\"cmd\":\"quit\"}\n").await;
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("dba-stack-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn picture(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x / 7) as u8, (y / 5) as u8, ((x ^ y) & 255) as u8]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

async fn wait_done(m: &DownloadManager, id: &str) -> decibell_attachments::manager::DownloadView {
    for _ in 0..1500 {
        if let Some(v) = m.list().into_iter().find(|v| v.id == id) {
            match v.state {
                DownloadState::Done => return v,
                DownloadState::Failed => panic!("download failed: {:?}", v.error),
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("download {id} never finished");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_stack_round_trip() {
    if std::env::var("DECIBELL_STACK").ok().as_deref() != Some("1") {
        eprintln!("SKIP: set DECIBELL_STACK=1 with tools/dev/local-stack.sh up");
        return;
    }
    let (mut helper, t) = Helper::start().await;
    let host = t["host"].as_str().unwrap().to_string();
    assert!(host.starts_with("127."), "loopback only: {host}");
    let svc: Arc<Attachments> = Attachments::new(Arc::new(TestTls), tokio::runtime::Handle::current());
    svc.set_target(
        "dg",
        AttachmentTarget {
            host: host.clone(),
            port: t["port"].as_u64().unwrap() as u16,
            jwt: t["jwt"].as_str().unwrap().into(),
            max_bytes: t["max_bytes"].as_u64().unwrap(),
            cert_fingerprint: t["fingerprint"].as_str().unwrap().into(),
        },
    );
    println!("Daily Gaming attachments at {host}:{} (cap {} bytes), pinned {}…", t["port"], t["max_bytes"], &t["fingerprint"].as_str().unwrap()[..16]);
    // #clips, not #general: #general's newest page is the fixture other tools capture.
    let general = t["clips"].as_str().unwrap().to_string();
    let dir = tmpdir("files");

    // ---- plain file + picture, bound to a real message
    let doc = plaintext(3 * 1024 * 1024 + 4321);
    let doc_path = dir.join("stack-test.bin");
    std::fs::write(&doc_path, &doc).unwrap();
    let pic = picture(1600, 1000);
    let pic_path = dir.join("stack-test.jpg");
    std::fs::write(&pic_path, &pic).unwrap();

    let ctl = UploadControl::new();
    let doc_prep = prepare(UploadSource::Path(doc_path.clone()), None).await.unwrap();
    let t0 = Instant::now();
    let doc_up = svc.upload("dg", &general, &doc_prep, false, &ctl, |_| {}).await.unwrap();
    println!("uploaded {} bytes plain as #{} in {:.2} s", doc.len(), doc_up.id, t0.elapsed().as_secs_f64());
    let pic_prep = prepare(UploadSource::Path(pic_path.clone()), None).await.unwrap();
    assert_eq!((pic_prep.width, pic_prep.height), (1600, 1000));
    let pic_up = svc.upload("dg", &general, &pic_prep, false, &ctl, |_| {}).await.unwrap();
    assert_eq!(pic_up.thumbnail_sizes_mask, 7, "three thumbnails accepted");
    let msg = helper.send(&general, "decibell-attachments stack test", &[doc_up.id, pic_up.id]).await;
    assert!(msg.get("error").is_none(), "{msg}");
    let msg_id = msg["id"].as_i64().unwrap();
    let atts = msg["attachments"].as_array().unwrap();
    println!("message #{} echoed with {} attachments: {}", msg["id"], atts.len(), msg["attachments"]);
    assert_eq!(atts.len(), 2);
    let a_doc = atts.iter().find(|a| a["id"] == doc_up.id).unwrap();
    assert_eq!((a_doc["filename"].as_str().unwrap(), a_doc["size_bytes"].as_u64().unwrap()), ("stack-test.bin", doc.len() as u64));
    let a_pic = atts.iter().find(|a| a["id"] == pic_up.id).unwrap();
    assert_eq!((a_pic["width"].as_u64().unwrap(), a_pic["height"].as_u64().unwrap()), (1600, 1000));
    assert_eq!(a_pic["thumbnail_sizes_mask"].as_u64().unwrap(), 7);
    assert_eq!(a_pic["placeholder"].as_str().unwrap(), pic_prep.placeholder);
    assert_eq!(a_pic["mime"].as_str().unwrap(), "image/jpeg");

    // ---- read back: whole, by range, a thumbnail
    assert_eq!(svc.fetch_all("dg", doc_up.id, None).await.unwrap(), doc);
    assert_eq!(svc.fetch_all("dg", pic_up.id, None).await.unwrap(), pic);
    for (s, e) in [(0u64, 0u64), (65535, 65536), (1_000_000, 2_999_999), (doc.len() as u64 - 10, doc.len() as u64 - 1)] {
        let mut st = svc.open("dg", doc_up.id, s, Some(e), None).await.unwrap();
        let mut got = Vec::new();
        st.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, &doc[s as usize..=e as usize], "range {s}-{e}");
    }
    let thumb = svc.fetch_thumbnail("dg", pic_up.id, 320, None).await.unwrap();
    let ti = image::load_from_memory(&thumb).unwrap();
    assert_eq!((ti.width(), ti.height()), (320, 200));

    // ---- speed caps, measured
    svc.limits().set_download(512 * 1024);
    let t0 = Instant::now();
    let mut st = svc.open("dg", doc_up.id, 0, Some(2 * 1024 * 1024 - 1), None).await.unwrap();
    let mut got = Vec::new();
    st.read_to_end(&mut got).await.unwrap();
    let rate = got.len() as f64 / t0.elapsed().as_secs_f64() / 1024.0;
    println!("download 2 MiB capped at 512 KiB/s: {rate:.0} KiB/s");
    assert!((440.0..=540.0).contains(&rate), "{rate}");
    svc.limits().set_download(0);
    svc.limits().set_upload(512 * 1024);
    let small = dir.join("capped-upload.bin");
    // Long enough that the bucket's head start (0.1 s of credit + one overdrawn 64 KiB slice,
    // as in Electron's pacer) doesn't dominate.
    std::fs::write(&small, plaintext(2 * 1024 * 1024)).unwrap();
    let small_prep = prepare(UploadSource::Path(small), None).await.unwrap();
    let t0 = Instant::now();
    svc.upload("dg", &general, &small_prep, false, &ctl, |_| {}).await.unwrap();
    let rate = 2048.0 / t0.elapsed().as_secs_f64();
    println!("upload 2 MiB capped at 512 KiB/s: {rate:.0} KiB/s");
    assert!((440.0..=560.0).contains(&rate), "{rate}");
    svc.limits().set_upload(0);

    // ---- downloads manager: paused, resumed, finished
    let state = tmpdir("state");
    let dl = tmpdir("dl");
    let m = DownloadManager::new(svc.clone(), ManagerOptions { state_dir: state, default_dir: dl.clone(), key_wrap: None }, |_| {});
    svc.limits().set_download(1024 * 1024);
    let start = |id: i64, name: &str, size: u64| StartArgs {
        server_id: "dg".into(),
        attachment_id: id,
        filename: name.into(),
        mime: "application/octet-stream".into(),
        kind: AttachmentKind::Document,
        size_bytes: size,
        context: DownloadContext { server_name: "Daily Gaming".into(), channel_id: general.clone(), ..Default::default() },
        destination: Destination::Folder,
    };
    let d = m.start(start(doc_up.id, "stack-test.bin", doc.len() as u64)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    m.pause(&d);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let v = m.list().into_iter().find(|v| v.id == d).unwrap();
    assert_eq!(v.state, DownloadState::Paused);
    let part_len = std::fs::metadata(dl.join("stack-test.bin.part")).unwrap().len();
    println!("paused at {part_len} of {} bytes", doc.len());
    assert!(part_len > 0 && part_len < doc.len() as u64);
    svc.limits().set_download(0);
    m.resume(&d);
    wait_done(&m, &d).await;
    assert_eq!(std::fs::read(dl.join("stack-test.bin")).unwrap(), doc);

    // ---- sealed: ciphertext up to #vault, back decrypted
    let vault = t["vault"].as_str().unwrap().to_string();
    assert!(t["vault_encrypted"].as_bool().unwrap(), "#vault is end-to-end encrypted");
    let secret = plaintext(2 * 1024 * 1024 + 99);
    let secret_path = dir.join("secret.pdf");
    std::fs::write(&secret_path, &secret).unwrap();
    let sp = prepare(UploadSource::Path(secret_path), None).await.unwrap();
    let up = svc.upload("dg", &vault, &sp, true, &ctl, |_| {}).await.unwrap();
    let meta = up.encrypted.clone().unwrap();
    assert_eq!((meta.filename.as_str(), meta.mime.as_str(), meta.chunk_bytes), ("secret.pdf", "application/pdf", CHUNK_BYTES as u32));
    // What the server holds is the ciphertext (read it without the key).
    let other = Attachments::new(Arc::new(TestTls), tokio::runtime::Handle::current());
    other.set_target("dg", svc.target("dg").unwrap());
    let raw = other.fetch_all("dg", up.id, None).await.unwrap();
    assert_eq!(raw.len() as u64, ciphertext_size(secret.len() as u64, CHUNK_BYTES));
    assert_ne!(&raw[..64], &secret[..64]);
    // With the key (registered by the upload, as the envelope would): plaintext, whole and ranged.
    assert_eq!(svc.fetch_all("dg", up.id, None).await.unwrap(), secret);
    for (s, e) in [(65535u64, 65536u64), (100_000, 1_500_000), (secret.len() as u64 - 1, secret.len() as u64 - 1)] {
        let mut st = svc.open("dg", up.id, s, Some(e), None).await.unwrap();
        let mut got = Vec::new();
        st.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, &secret[s as usize..=e as usize], "sealed range {s}-{e}");
    }
    let d = m.start(start(up.id, "secret.pdf", 0)).await.unwrap();
    let v = wait_done(&m, &d).await;
    assert!(v.encrypted);
    assert_eq!(std::fs::read(dl.join("secret.pdf")).unwrap(), secret);
    println!("sealed #{}: {} plaintext bytes = {} on the server, read back whole, by range and by the manager", up.id, secret.len(), raw.len());

    // Leave the fixture as it was: delete our message (its attachments go with it).
    let del = helper.delete(&general, msg_id).await;
    assert_eq!(del["success"], Value::Bool(true), "{del}");
    helper.quit().await;
}
