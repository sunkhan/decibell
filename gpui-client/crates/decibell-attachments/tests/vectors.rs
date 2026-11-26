//! Cross-implementation vectors (tests/vectors/): the Electron client's own TS modules made
//! them (`node tests/vectors/gen_vectors.mjs gen tests/vectors/data`).
//!
//! * committed: digests of the renderer's sealing for every size, the small sealed files, range
//!   plans + plaintext digests from main's opener, sealed thumbnails, ThumbHash bytes;
//! * at test time (when node + electron-client/node_modules are there): every renderer-sealed
//!   file including 5 MiB is regenerated and opened here, served through the mock server and
//!   read by range through the streaming decryptor; what Rust seals is opened by main's TS
//!   decryptor.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use decibell_attachments::crypto::{self, ciphertext_size, parse_range, plan_range, CHUNK_BYTES};
use decibell_attachments::{thumbhash, AttachmentKey, FileKey};
use serde_json::Value;
use support::*;

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/data")
}

fn vectors() -> Value {
    serde_json::from_str(&std::fs::read_to_string(data_dir().join("vectors.json")).unwrap()).unwrap()
}

fn b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

#[test]
fn rust_sealing_is_byte_identical_to_the_renderer() {
    let v = vectors();
    assert_eq!(v["chunkBytes"].as_u64(), Some(CHUNK_BYTES));
    for s in v["sealed"].as_array().unwrap() {
        let size = s["size"].as_u64().unwrap();
        let key = FileKey::from_b64(s["keyB64"].as_str().unwrap()).unwrap();
        assert_eq!(key.as_bytes(), &key_for(size));
        let plain = plaintext(size as usize);
        assert_eq!(sha256_hex(&plain), s["plainSha256"].as_str().unwrap(), "plaintext stream, size {size}");
        let sealed = key.seal_all(&plain);
        assert_eq!(sealed.len() as u64, s["sealedLen"].as_u64().unwrap());
        assert_eq!(sha256_hex(&sealed), s["sealedSha256"].as_str().unwrap(), "sealed bytes differ at size {size}");
        assert_eq!(ciphertext_size(size, CHUNK_BYTES), sealed.len() as u64);
        println!("size {size:>8}: renderer seal == Rust seal ({} bytes)", sealed.len());
    }
}

#[test]
fn rust_opens_the_committed_renderer_files() {
    let v = vectors();
    for s in v["sealed"].as_array().unwrap() {
        let size = s["size"].as_u64().unwrap();
        let path = data_dir().join(format!("sealed-{size}.bin"));
        if !path.exists() {
            continue;
        }
        let sealed = std::fs::read(&path).unwrap();
        let key = FileKey::from_b64(s["keyB64"].as_str().unwrap()).unwrap();
        let plain = key.open_all(&sealed, size, CHUNK_BYTES).unwrap();
        assert_eq!(sha256_hex(&plain), s["plainSha256"].as_str().unwrap());
    }
    // A key from the renderer's own random generator.
    let r = &v["randomKey"];
    let sealed = std::fs::read(data_dir().join(r["file"].as_str().unwrap())).unwrap();
    let key = FileKey::from_b64(r["keyB64"].as_str().unwrap()).unwrap();
    let plain = key.open_all(&sealed, r["size"].as_u64().unwrap(), CHUNK_BYTES).unwrap();
    assert_eq!(sha256_hex(&plain), r["plainSha256"].as_str().unwrap());
}

#[test]
fn thumbnails_match_both_ways() {
    for t in vectors()["thumbnails"].as_array().unwrap() {
        let size_px = t["sizePx"].as_u64().unwrap() as u32;
        let key = FileKey::from_b64(t["keyB64"].as_str().unwrap()).unwrap();
        let sealed = b64(t["sealedB64"].as_str().unwrap());
        let jpeg = key.open_thumbnail(size_px, &sealed).unwrap();
        assert_eq!(sha256_hex(&jpeg), t["jpegSha256"].as_str().unwrap());
        // Deterministic nonce: our seal is the renderer's seal.
        assert_eq!(key.seal_thumbnail(size_px, &jpeg), sealed);
    }
}

#[test]
fn range_plans_match_main() {
    for r in vectors()["ranges"].as_array().unwrap() {
        let size = r["size"].as_u64().unwrap();
        let header = r["header"].as_str();
        let parsed = header.and_then(|h| parse_range(h, size));
        match (&r["parsed"], parsed) {
            (Value::Null, None) => {
                if header.is_some() {
                    continue; // unsatisfiable: main answers 416, so do we
                }
            }
            (Value::Array(a), Some((s, e))) => {
                assert_eq!((a[0].as_u64().unwrap(), a[1].as_u64().unwrap()), (s, e), "{header:?}");
            }
            (want, got) => panic!("{header:?}: main parsed {want}, we parsed {got:?}"),
        }
        let p = plan_range(size, CHUNK_BYTES, parsed);
        let want = &r["plan"];
        assert_eq!(p.first_chunk, want["firstChunk"].as_u64().unwrap(), "{header:?}");
        assert_eq!(p.last_chunk, want["lastChunk"].as_u64().unwrap(), "{header:?}");
        assert_eq!(p.trim_start, want["trimStart"].as_u64().unwrap(), "{header:?}");
        assert_eq!(p.trim_end, want["trimEnd"].as_u64().unwrap(), "{header:?}");
        assert_eq!(p.plain_start, want["plainStart"].as_u64().unwrap());
        assert_eq!(p.plain_end, want["plainEnd"].as_u64().unwrap());
        let up = p.upstream.map(|(a, b)| format!("bytes={a}-{b}"));
        assert_eq!(up.as_deref(), want["upstreamRange"].as_str(), "{header:?}");
    }
}

#[test]
fn thumbhash_is_bit_identical() {
    for t in vectors()["thumbhash"].as_array().unwrap() {
        let (w, h) = (t["w"].as_u64().unwrap() as usize, t["h"].as_u64().unwrap() as usize);
        let kind = t["kind"].as_str().unwrap();
        let rgba = image(kind, w, h);
        assert_eq!(sha256_hex(&rgba), t["rgbaSha256"].as_str().unwrap(), "test image {kind} {w}x{h}");
        let hash = thumbhash::encode(w, h, &rgba);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(&hash),
            t["hashB64"].as_str().unwrap(),
            "encode {kind} {w}x{h}"
        );
        let d = thumbhash::decode(&hash);
        let want = &t["decoded"];
        assert_eq!((d.width as u64, d.height as u64), (want["w"].as_u64().unwrap(), want["h"].as_u64().unwrap()));
        assert_eq!(sha256_hex(&d.rgba), want["sha256"].as_str().unwrap(), "decode {kind} {w}x{h}");
        println!("thumbhash {kind:>10} {w:>3}x{h:<3} {} == npm", t["hashB64"].as_str().unwrap());
    }
}

#[test]
fn thumbhash_fuzz_is_bit_identical() {
    let v = vectors();
    println!("vectors made by {}", v["engine"]);
    let imgs = v["fuzzImages"].as_array().unwrap();
    let mut bad = Vec::new();
    for t in imgs {
        let (w, h) = (t["w"].as_u64().unwrap() as usize, t["h"].as_u64().unwrap() as usize);
        let rgba = fuzz_image(t["seed"].as_u64().unwrap() as u32, w, h, t["alpha"].as_bool().unwrap());
        let got = base64::engine::general_purpose::STANDARD.encode(thumbhash::encode(w, h, &rgba));
        if got != t["hashB64"].as_str().unwrap() {
            bad.push(format!("encode {w}x{h}: {got} != {}", t["hashB64"]));
        }
    }
    let hashes = v["fuzzHashes"].as_array().unwrap();
    for t in hashes {
        let d = thumbhash::decode(&b64(t["hashB64"].as_str().unwrap()));
        if (d.width as u64, d.height as u64) != (t["w"].as_u64().unwrap(), t["h"].as_u64().unwrap())
            || sha256_hex(&d.rgba) != t["sha256"].as_str().unwrap()
        {
            bad.push(format!("decode {}", t["hashB64"]));
        }
    }
    println!("thumbhash fuzz: {} images encoded, {} hashes decoded, {} differ", imgs.len(), hashes.len(), bad.len());
    assert!(bad.is_empty(), "{bad:#?}");
}

/// vectors.ts's `fuzzImage()`.
fn fuzz_image(seed: u32, w: usize, h: usize, alpha: bool) -> Vec<u8> {
    let mut next = xorshift(seed);
    let base = [next() & 255, next() & 255, next() & 255];
    let mut px = vec![0u8; w * h * 4];
    for i in 0..w * h {
        let v = next();
        px[i * 4] = ((base[0] + (v & 63)) & 255) as u8;
        px[i * 4 + 1] = ((base[1] + ((v >> 6) & 63) + (i % w) as u32) & 255) as u8;
        px[i * 4 + 2] = ((base[2] + ((v >> 12) & 63)) & 255) as u8;
        px[i * 4 + 3] = if alpha { (v >> 24) as u8 } else { 255 };
    }
    px
}

/// vectors.ts's `image()`.
fn image(kind: &str, w: usize, h: usize) -> Vec<u8> {
    let mut px = vec![0u8; w * h * 4];
    let mut next = xorshift(0x123_4567 ^ (w as u32 * 131 + h as u32));
    let ramp = |v: usize, n: usize| ((v * 255) as f64 / (n.max(2) - 1) as f64).floor() as u8;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let p = &mut px[i..i + 4];
            match kind {
                "gradient" => p.copy_from_slice(&[ramp(x, w), ramp(y, h), 128, 255]),
                "noise" => {
                    let v = next();
                    p.copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]);
                }
                "alpha" => p.copy_from_slice(&[220, ramp(x, w), 40, ramp(y, h)]),
                "noisealpha" => p.copy_from_slice(&next().to_le_bytes()),
                "clear" => p.copy_from_slice(&[0, 0, 0, 0]),
                _ => p.copy_from_slice(&[30, 144, 255, 255]),
            }
        }
    }
    px
}

/// Every size (5 MiB included) sealed by the renderer module now, opened here, then served by
/// the mock server and read by range through the streaming decryptor; and our sealing opened
/// by main's decryptor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn big_vectors_both_ways_and_streamed_ranges() {
    let Some(mut gen) = js_runtime() else {
        eprintln!("SKIP: no JS runtime or electron-client/node_modules/esbuild");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("dba-vectors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let out = gen.arg("gen").arg(&tmp).arg("--big").output().unwrap();
    assert!(out.status.success(), "gen: {}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(tmp.join("vectors.json")).unwrap()).unwrap();

    // 1. Renderer → Rust, whole files.
    for s in v["sealed"].as_array().unwrap() {
        let size = s["size"].as_u64().unwrap();
        let sealed = std::fs::read(tmp.join(format!("sealed-{size}.bin"))).unwrap();
        let key = FileKey::from_b64(s["keyB64"].as_str().unwrap()).unwrap();
        let plain = key.open_all(&sealed, size, CHUNK_BYTES).unwrap();
        assert_eq!(sha256_hex(&plain), s["plainSha256"].as_str().unwrap(), "size {size}");
    }

    // 2. Renderer-sealed 200 KiB served by the mock; ranges through the streaming decryptor
    //    must give main's plaintext digests.
    let mock = Mock::start().await;
    let svc = service();
    svc.set_target("srv", mock.target());
    let mut ids = std::collections::HashMap::new();
    for size in [200 * 1024u64, 65537] {
        let sealed = std::fs::read(tmp.join(format!("sealed-{size}.bin"))).unwrap();
        let id = mock.seed(sealed, true);
        svc.register_key(
            "srv",
            id,
            AttachmentKey { key: FileKey::from_bytes(key_for(size)), chunk_bytes: CHUNK_BYTES, size_bytes: size, mime: "x/y".into(), filename: "f".into() },
        );
        ids.insert(size, id);
    }
    let mut checked = 0;
    for r in v["ranges"].as_array().unwrap() {
        let size = r["size"].as_u64().unwrap();
        let Some(want) = r["plainSha256"].as_str() else { continue };
        let header = r["header"].as_str();
        let (start, end) = match header {
            None => (0, None),
            Some(h) => {
                let (s, e) = parse_range(h, size).unwrap();
                (s, Some(e))
            }
        };
        let mut stream = svc.open("srv", ids[&size], start, end, None).await.unwrap();
        let mut got = Vec::new();
        stream.read_to_end(&mut got).await.unwrap();
        assert_eq!(got.len() as u64, r["plainLen"].as_u64().unwrap(), "{header:?}");
        assert_eq!(sha256_hex(&got), want, "streamed range {header:?} of {size}");
        checked += 1;
    }
    println!("{checked} ranges streamed through the decryptor == main's planRange + decryptChunk");
    // Range reads reuse one keep-alive connection.
    assert!(mock.connections.load(std::sync::atomic::Ordering::SeqCst) <= 2, "connections: {}", mock.connections.load(std::sync::atomic::Ordering::SeqCst));

    // 3. Rust → main's TS decryptor.
    let rust_dir = tmp.join("rust");
    std::fs::create_dir_all(&rust_dir).unwrap();
    let mut files = Vec::new();
    for size in [0u64, 1, 65535, 65536, 65537, 200 * 1024, 5 * 1024 * 1024, 3 * 65536 + 5] {
        let key = FileKey::generate();
        let plain = plaintext(size as usize);
        let sealed = key.seal_all(&plain);
        let file = format!("rust-{size}.bin");
        std::fs::write(rust_dir.join(&file), &sealed).unwrap();
        files.push(serde_json::json!({"file": file, "size": size, "keyB64": key.to_b64(), "plainSha256": sha256_hex(&plain)}));
    }
    let mut thumbs = Vec::new();
    for size_px in [320u32, 640, 1280] {
        let key = FileKey::generate();
        let jpeg = plaintext(5000 + size_px as usize);
        thumbs.push(serde_json::json!({"sizePx": size_px, "keyB64": key.to_b64(),
            "sealedB64": base64::engine::general_purpose::STANDARD.encode(key.seal_thumbnail(size_px, &jpeg)),
            "jpegSha256": sha256_hex(&jpeg)}));
    }
    std::fs::write(rust_dir.join("manifest.json"), serde_json::json!({"files": files, "thumbnails": thumbs}).to_string()).unwrap();
    let out = js_runtime().unwrap().arg("verify").arg(&rust_dir).output().unwrap();
    assert!(out.status.success(), "verify: {}", String::from_utf8_lossy(&out.stderr));
    let res: Value = serde_json::from_slice(&out.stdout).unwrap();
    println!("main's decryptor on Rust output: {}", res);
    assert_eq!(res["ok"], Value::Bool(true));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = Arc::strong_count(&svc);
    let _ = crypto::TAG_BYTES;
}
