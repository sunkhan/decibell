//! A large sealed transfer, to show it streams: `DECIBELL_BIG=1 cargo test --release
//! --test bigfile -- --nocapture` (size in MiB via `DECIBELL_BIG_MIB`, default 200). The mock
//! server runs here; the client side (seal + upload from disk, then the downloads manager
//! fetching, decrypting and writing to disk) runs in a child process (this binary again), whose
//! peak RSS (`VmHWM`) is the number that matters. Linux only (reads /proc).

mod support;

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use support::*;

fn vm(field: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[test]
fn big_sealed_transfer_streams() {
    if std::env::var("DECIBELL_BIG").is_err() {
        eprintln!("SKIP: DECIBELL_BIG=1 (with --release) runs the large transfer");
        return;
    }
    let mib: u64 = std::env::var("DECIBELL_BIG_MIB").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        let mock = Mock::start().await;
        let dir = std::env::temp_dir().join(format!("dba-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The plaintext file, written in 1 MiB pieces.
        let src = dir.join("big.bin");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&src).unwrap());
        let mut next = xorshift(42);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        for _ in 0..mib {
            for chunk in buf.chunks_mut(4) {
                chunk.copy_from_slice(&next().to_le_bytes());
            }
            hasher.update(&buf);
            f.write_all(&buf).unwrap();
        }
        f.flush().unwrap();
        drop(f);
        let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let exe = std::env::current_exe().unwrap();
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new(exe)
                .args(["--exact", "client_side", "--ignored", "--nocapture", "--test-threads=1"])
                .env("DBA_PORT", mock.addr.port().to_string())
                .env("DBA_DIR", &dir)
                .env("DBA_SHA", &digest)
                .env("DBA_FP", test_cert_fingerprint())
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().filter(|l| l.contains("BIG ")) {
            println!("{line}");
        }
        assert!(out.status.success(), "child: {}\n{}", text, String::from_utf8_lossy(&out.stderr));
        assert!(text.contains("BIG ok"), "{text}");
    });
}

/// The client side of the run above (only when started by it).
#[test]
#[ignore]
fn client_side() {
    let Ok(port) = std::env::var("DBA_PORT") else { return };
    let dir = std::path::PathBuf::from(std::env::var("DBA_DIR").unwrap());
    let want = std::env::var("DBA_SHA").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async move {
        use decibell_attachments::manager::*;
        use decibell_attachments::upload::*;
        let base = vm("VmRSS:");
        let svc = decibell_attachments::Attachments::new(Arc::new(TestTls), tokio::runtime::Handle::current());
        svc.set_target(
            "srv",
            decibell_attachments::AttachmentTarget {
                host: "127.0.0.1".into(),
                port: port.parse().unwrap(),
                jwt: TOKEN.into(),
                max_bytes: 0,
                cert_fingerprint: test_cert_fingerprint(),
            },
        );
        let src = dir.join("big.bin");
        let size = std::fs::metadata(&src).unwrap().len();
        let prepared = prepare(UploadSource::Path(src.clone()), None).await.unwrap();
        let t0 = Instant::now();
        let mut calls = 0usize;
        let up = svc.upload("srv", "general", &prepared, true, &UploadControl::new(), |_| calls += 1).await.unwrap();
        let up_s = t0.elapsed().as_secs_f64();
        let after_up = vm("VmHWM:");
        println!(
            "BIG upload  {} MiB sealed: {up_s:.2} s = {:.0} MiB/s, {calls} progress calls, peak RSS {} MiB (start {} MiB)",
            size >> 20,
            size as f64 / up_s / 1048576.0,
            after_up / 1024,
            base / 1024
        );

        let out_dir = dir.join("out");
        let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let e2 = events.clone();
        let m = DownloadManager::new(
            svc.clone(),
            ManagerOptions { state_dir: dir.join("state"), default_dir: out_dir.clone(), key_wrap: None },
            move |_| {
                e2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            },
        );
        let t0 = Instant::now();
        let id = m
            .start(StartArgs {
                server_id: "srv".into(),
                attachment_id: up.id,
                filename: "big.bin".into(),
                mime: "application/octet-stream".into(),
                kind: decibell_attachments::AttachmentKind::Document,
                size_bytes: size,
                context: DownloadContext::default(),
                destination: Destination::Folder,
            })
            .await
            .unwrap();
        loop {
            let v = m.list().into_iter().find(|v| v.id == id).unwrap();
            match v.state {
                DownloadState::Done => break,
                DownloadState::Failed => panic!("{:?}", v.error),
                _ => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let down_s = t0.elapsed().as_secs_f64();
        let peak = vm("VmHWM:");
        println!(
            "BIG download {} MiB sealed (manager, decrypt to disk): {down_s:.2} s = {:.0} MiB/s, {} events, peak RSS {} MiB",
            size >> 20,
            size as f64 / down_s / 1048576.0,
            events.load(std::sync::atomic::Ordering::Relaxed),
            peak / 1024
        );
        let mut f = std::fs::File::open(out_dir.join("big.bin")).unwrap();
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, want);
        println!("BIG ok: downloaded file == source (sha256 {}…)", &got[..16]);
        let _ = std::fs::remove_dir_all(&dir);
    });
}
