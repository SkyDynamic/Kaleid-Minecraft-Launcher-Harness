//! 下载重试 / 分段 / 换源的行为验收（不联网：本地起一个最小的 HTTP/1.1 服务）。
//!
//! 起因是一次真实故障：装 Fabric 时「Fabric API」只是一个文件，于是整批只剩一个
//! 线程在跑，连接一到超时就抛裸 `(os error 10060)`，整个安装报废。这里把那几件
//! 事钉住：
//!
//! 1. 连接被掐断 / 服务器 5xx → 退避重试后成功；
//! 2. 服务器忽略 `Range` → 退回单流仍然成功（不能因为分段失败就判死）；
//! 3. 服务器支持 `Range` → 真的并发多连接取同一个文件，拼接后 SHA-1 一致；
//! 4. SHA-1 不符 → 不重试（内容已经完整拿到，再请求只会再拿到错的东西）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use sha1::{Digest, Sha1};

use kaleid_api::{LoaderKind, SettingsServiceApi};
use kaleid_core_plugins::downloads::{DownloadItem, Job};
use kaleid_core_plugins::{DownloadManager, Settings};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 前 n 个连接直接断开（模拟连接被掐断 / 中途丢失）。
    DropFirst(usize),
    /// 前 n 个请求回 500。
    Status500First(usize),
    /// 忽略 `Range`：永远回整个文件。
    IgnoreRange,
    /// 正常支持 `Range`。
    RangeOk,
    /// 正常支持 `Range`，但按块慢速写：这样才观察得到"同时有几个连接在跑"。
    SlowRange { chunk: usize, delay_ms: u64 },
    /// 前 n 个请求回 429（限流），可以带 `Retry-After`。
    Throttle { times: usize, retry_after: Option<u64> },
}

struct TestServer {
    base: String,
    requests: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    fn start(body: Vec<u8>, mode: Mode) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body = Arc::new(body);
        let requests = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let requests = requests.clone();
            let peak = peak.clone();
            let active = active.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let body = body.clone();
                            let requests = requests.clone();
                            let peak = peak.clone();
                            let active = active.clone();
                            // 每个连接一个线程：这样才测得出"同时开了几个连接"。
                            std::thread::spawn(move || {
                                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                                peak.fetch_max(now, Ordering::SeqCst);
                                let index = requests.fetch_add(1, Ordering::SeqCst);
                                serve(stream, &body, mode, index);
                                active.fetch_sub(1, Ordering::SeqCst);
                            });
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        TestServer {
            base,
            requests,
            peak,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn serve(mut stream: TcpStream, body: &[u8], mode: Mode, index: usize) {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let head = match read_head(&mut stream) {
        Some(head) => head,
        None => return,
    };

    if let Mode::DropFirst(n) = mode {
        if index < n {
            // 什么都不写直接关掉：客户端会看到"连接在响应前就没了"。
            return;
        }
    }
    if let Mode::Status500First(n) = mode {
        if index < n {
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
    }
    if let Mode::Throttle { times, retry_after } = mode {
        if index < times {
            // 真实的限流响应长这样：没有 body，可能带 Retry-After 或配额重置时间。
            let extra = match retry_after {
                Some(seconds) => format!(
                    "Retry-After: {seconds}\r\nX-RateLimit-Reset-short: {seconds}\r\n"
                ),
                None => String::new(),
            };
            let response = format!(
                "HTTP/1.1 429 Too Many Requests\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            return;
        }
    }

    let range = if mode == Mode::IgnoreRange {
        None
    } else {
        parse_range(&head)
    };
    let throttle = match mode {
        Mode::SlowRange { chunk, delay_ms } => Some((chunk, delay_ms)),
        _ => None,
    };
    match range {
        Some((start, end)) => write_partial(&mut stream, body, start, end, throttle),
        None => write_full(&mut stream, body, throttle),
    }
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return None,
            Ok(n) => {
                head.extend_from_slice(&buf[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") || head.len() > 64 * 1024 {
                    return Some(String::from_utf8_lossy(&head).to_string());
                }
            }
            Err(_) => return None,
        }
    }
}

fn parse_range(head: &str) -> Option<(u64, u64)> {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("range") {
            continue;
        }
        let spec = value.trim().strip_prefix("bytes=")?;
        let (start, end) = spec.split_once('-')?;
        return Some((start.trim().parse().ok()?, end.trim().parse().ok()?));
    }
    None
}

fn write_full(stream: &mut TcpStream, body: &[u8], throttle: Option<(usize, u64)>) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    write_body(stream, body, throttle);
}

fn write_partial(
    stream: &mut TcpStream,
    body: &[u8],
    start: u64,
    end: u64,
    throttle: Option<(usize, u64)>,
) {
    let total = body.len() as u64;
    if start >= total {
        let _ = stream.write_all(
            b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let end = end.min(total - 1);
    let slice = &body[start as usize..=end as usize];
    let head = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        slice.len()
    );
    let _ = stream.write_all(head.as_bytes());
    write_body(stream, slice, throttle);
}

/// 写响应体；给了 throttle 就慢写，用来制造"多个连接同时在跑"的窗口。
fn write_body(stream: &mut TcpStream, body: &[u8], throttle: Option<(usize, u64)>) {
    match throttle {
        Some((chunk, delay_ms)) => {
            for piece in body.chunks(chunk.max(1)) {
                if stream.write_all(piece).is_err() {
                    return;
                }
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
        }
        None => {
            let _ = stream.write_all(body);
            let _ = stream.flush();
        }
    }
}

/* ---------------------------- 公共小工具 ---------------------------- */

fn body_of(len: usize) -> Vec<u8> {
    // 可复现的伪随机内容：不同段落内容不同，拼接错位会被 SHA-1 抓到。
    (0..len).map(|i| ((i * 37 + i / 251) % 251) as u8).collect()
}

fn sha1_hex(bytes: &[u8]) -> String {
    hex::encode(Sha1::digest(bytes))
}

fn manager(dir: &Path) -> DownloadManager {
    let settings = Arc::new(Settings::open(dir));
    DownloadManager::new(settings)
}

fn manager_with_concurrency(dir: &Path, concurrency: u32) -> DownloadManager {
    let settings = Arc::new(Settings::open(dir));
    settings
        .patch_global(&serde_json::json!({ "download_concurrency": concurrency }))
        .expect("设置下载并发");
    DownloadManager::new(settings)
}

fn job(id: &str) -> Arc<Job> {
    Job::new(
        id.into(),
        "测试任务".into(),
        "1.21.4-fabric".into(),
        "roaming".into(),
        "1.21.4".into(),
        LoaderKind::Vanilla,
        None,
        "official-first".into(),
    )
}

fn item(label: &str, url: String, dest: &Path, body: &[u8]) -> DownloadItem {
    DownloadItem {
        label: label.into(),
        phase: "fabric-api".into(),
        official_url: Some(url),
        mirror_url: None,
        fallback_url: None,
        prefer_mirror: false,
        dest: dest.to_path_buf(),
        size: body.len() as u64,
        sha1: Some(sha1_hex(body)),
    }
}

/// 官方 + 镜像两个地址的文件：用来验证"什么时候该换源、什么时候不该"。
fn item_with_mirror(
    label: &str,
    official: String,
    mirror: String,
    dest: &Path,
    body: &[u8],
) -> DownloadItem {
    DownloadItem {
        label: label.into(),
        phase: "vanilla".into(),
        official_url: Some(official),
        mirror_url: Some(mirror),
        fallback_url: None,
        prefer_mirror: false,
        dest: dest.to_path_buf(),
        size: body.len() as u64,
        sha1: Some(sha1_hex(body)),
    }
}

fn assert_no_leftovers(dest: &Path) {
    assert!(!dest.exists() || dest.is_file());
    let dir = dest.parent().unwrap();
    let leftovers = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".part"))
        .count();
    assert_eq!(leftovers, 0, "不应留下 .part 残留");
}

/* ------------------------------- 用例 ------------------------------- */

#[test]
fn retries_after_connection_drops() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(64 * 1024);
    let server = TestServer::start(body.clone(), Mode::DropFirst(1));
    let manager = manager(dir.path());
    let dest = dir.path().join("fabric-api-0.0.0.jar");

    manager
        .run_batch(
            job("drop"),
            vec![item("Fabric API", format!("{}/api.jar", server.base), &dest, &body)],
        )
        .expect("连接被掐断后应当自动重试并成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_no_leftovers(&dest);
    assert!(
        server.requests.load(Ordering::SeqCst) >= 2,
        "应当至少重试一次"
    );
}

#[test]
fn retries_after_server_errors() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(32 * 1024);
    let server = TestServer::start(body.clone(), Mode::Status500First(2));
    let manager = manager(dir.path());
    let dest = dir.path().join("api.jar");

    manager
        .run_batch(
            job("500"),
            vec![item("Fabric API", format!("{}/api.jar", server.base), &dest, &body)],
        )
        .expect("5xx 应当重试后成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_no_leftovers(&dest);
}

#[test]
fn falls_back_to_single_stream_when_range_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    // 超过 1 MiB 才会尝试分段，这里刻意跨过阈值。
    let body = body_of(2 * 1024 * 1024 + 1234);
    let server = TestServer::start(body.clone(), Mode::IgnoreRange);
    let manager = manager(dir.path());
    let dest = dir.path().join("api.jar");

    manager
        .run_batch(
            job("ignore-range"),
            vec![item("Fabric API", format!("{}/api.jar", server.base), &dest, &body)],
        )
        .expect("服务器忽略 Range 时应当退回单流并成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_no_leftovers(&dest);
    // 分段的第一批请求 + 退回单流至少一次。
    assert!(server.requests.load(Ordering::SeqCst) >= 2);
}

#[test]
fn downloads_large_file_with_several_connections() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(2 * 1024 * 1024 + 7);
    let server = TestServer::start(body.clone(), Mode::RangeOk);
    let manager = manager(dir.path());
    let dest = dir.path().join("api.jar");

    manager
        .run_batch(
            job("segments"),
            vec![item("Fabric API", format!("{}/api.jar", server.base), &dest, &body)],
        )
        .expect("分段下载应当成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_no_leftovers(&dest);
    assert!(
        server.peak.load(Ordering::SeqCst) >= 2,
        "应当同时开多个连接（实测峰值 {}）",
        server.peak.load(Ordering::SeqCst)
    );
    assert!(
        server.requests.load(Ordering::SeqCst) >= 2,
        "分段应当拆成多次 Range 请求"
    );
}

#[test]
fn honors_retry_after_and_keeps_downloading_the_same_url() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(16 * 1024);
    // 第一个请求 429 + Retry-After: 1，之后恢复正常。
    let official = TestServer::start(body.clone(), Mode::Throttle { times: 1, retry_after: Some(1) });
    let mirror = TestServer::start(body.clone(), Mode::RangeOk);
    let manager = manager(dir.path());
    let dest = dir.path().join("client.jar");

    let started = std::time::Instant::now();
    manager
        .run_batch(
            job("throttle-429"),
            vec![item_with_mirror(
                "客户端",
                format!("{}/client.jar", official.base),
                format!("{}/client.jar", mirror.base),
                &dest,
                &body,
            )],
        )
        .expect("限流应当退避重试后成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_no_leftovers(&dest);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "应当按 Retry-After 等够 1 秒（实测 {:?}）",
        started.elapsed()
    );
    // 关键：429 ≠ 换源。镜像一次都不该被访问。
    assert_eq!(
        mirror.requests.load(Ordering::SeqCst),
        0,
        "限流时不应换源，镜像被访问了 {} 次",
        mirror.requests.load(Ordering::SeqCst)
    );
    assert!(official.requests.load(Ordering::SeqCst) >= 2);
}

#[test]
fn gives_up_on_a_persistently_throttled_source_and_switches() {
    // 让退避缩到毫秒级，否则"三轮 × 三次退避"要跑二十多秒。
    std::env::set_var("KALEID_THROTTLE_BACKOFF_MS", "20");
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(32 * 1024);
    // 官方源永久限流，镜像正常。
    let official = TestServer::start(
        body.clone(),
        Mode::Throttle {
            times: usize::MAX,
            retry_after: None,
        },
    );
    let mirror = TestServer::start(body.clone(), Mode::RangeOk);
    let manager = manager(dir.path());
    let dest = dir.path().join("client.jar");

    manager
        .run_batch(
            job("throttle-forever"),
            vec![item_with_mirror(
                "客户端",
                format!("{}/client.jar", official.base),
                format!("{}/client.jar", mirror.base),
                &dest,
                &body,
            )],
        )
        .expect("官方源被限死之后应当换到镜像并成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_no_leftovers(&dest);
    assert!(
        official.requests.load(Ordering::SeqCst) >= 3,
        "换源前应当先在官方源上重试过（实测 {} 次）",
        official.requests.load(Ordering::SeqCst)
    );
    assert!(
        mirror.requests.load(Ordering::SeqCst) >= 1,
        "官方源彻底限流后应当改用镜像"
    );
}

#[test]
fn digest_mismatch_fails_without_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(8 * 1024);
    let server = TestServer::start(body.clone(), Mode::RangeOk);
    let manager = manager(dir.path());
    let dest = dir.path().join("api.jar");

    let mut broken = item("Fabric API", format!("{}/api.jar", server.base), &dest, &body);
    broken.sha1 = Some("0".repeat(40));

    let err = manager
        .run_batch(job("digest"), vec![broken])
        .expect_err("SHA-1 不符必须失败");
    let message = err.to_string();
    assert!(message.contains("SHA-1"), "{message}");
    // 内容已经完整拿到，重试只会再拿到同样的错内容——只请求一次。
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert!(!dest.exists(), "校验失败不能留下目标文件");
    assert_no_leftovers(&dest);
}

#[test]
fn unreachable_address_fails_with_plain_chinese_hint() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(1024);
    let manager = manager(dir.path());
    let dest = dir.path().join("api.jar");
    // 端口 1 上没人监听：连接会被立刻拒绝，走的是「连不上 → 换源」那条路。
    let mut failing = item("Fabric API", "http://127.0.0.1:1/api.jar".into(), &dest, &body);
    failing.sha1 = None;

    let err = manager
        .run_batch(job("unreachable"), vec![failing])
        .expect_err("连不上就必须失败");
    let message = err.to_string();
    assert!(message.contains("Fabric API"), "{message}");
    // 错误串里要有给人看的原因，而不是只有裸的 io 错误。
    assert!(
        message.contains("连接") || message.contains("拒绝") || message.contains("原始"),
        "{message}"
    );
}


/// 快照里的「线程数」必须反映真实的下载连接数。
///
/// 起因是一张用户截图：装 Fabric 到 99.8% 时只剩「Fabric API」一个文件，界面写着
/// 「1/64 线程」，看起来"有 63 个线程空着却只用一个"。实际上下载引擎确实切了分段，
/// 但快照只数线程池里忙碌的 worker——分段起的连接在界面上完全看不见。
#[test]
fn snapshot_reports_segment_connections_while_downloading() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(4 * 1024 * 1024);
    let server = TestServer::start(
        body.clone(),
        Mode::SlowRange {
            chunk: 32 * 1024,
            delay_ms: 10,
        },
    );
    let manager = manager(dir.path());
    let dest = dir.path().join("api.jar");

    let watched = manager.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut peak = 0u32;
            while !stop.load(Ordering::SeqCst) {
                peak = peak.max(watched.snapshot().active_workers);
                std::thread::sleep(Duration::from_millis(10));
            }
            peak
        })
    };

    manager
        .run_batch(
            job("visible"),
            vec![item("Fabric API", format!("{}/api.jar", server.base), &dest, &body)],
        )
        .expect("分段下载应当成功");
    stop.store(true, Ordering::SeqCst);
    let peak = poller.join().unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert!(
        peak >= 2,
        "快照里的线程数应当数到分段连接，实测峰值 {peak}"
    );
}

/// 只剩一个文件时，空闲的线程要能被它用上。
///
/// 旧实现把分段数固定成 `min(并发/2, 4)`：并发设成 32、只剩一个文件时也只开 4 个连接。
/// 现在按"当下有多少空闲线程"决定分段数，单文件能把空闲线程吃掉（上限 8）。
#[test]
fn spare_threads_are_used_for_one_large_file() {
    let dir = tempfile::tempdir().unwrap();
    let body = body_of(8 * 1024 * 1024);
    let server = TestServer::start(
        body.clone(),
        Mode::SlowRange {
            chunk: 64 * 1024,
            delay_ms: 10,
        },
    );
    let manager = manager_with_concurrency(dir.path(), 32);
    let dest = dir.path().join("api.jar");

    manager
        .run_batch(
            job("spare"),
            vec![item("Fabric API", format!("{}/api.jar", server.base), &dest, &body)],
        )
        .expect("分段下载应当成功");

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let peak = server.peak.load(Ordering::SeqCst);
    assert!(
        peak > 8,
        "并发 32 只剩一个文件时不该再停在 8 段上限，实测峰值 {peak}"
    );
}