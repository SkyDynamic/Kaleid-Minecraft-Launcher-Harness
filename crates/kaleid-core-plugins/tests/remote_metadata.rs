//! 元数据抓取的验收：镜像返回"200 + 一段 HTML 错误页"时，不能只报一句
//! `expected value at line 1 column 1`。
//!
//! 真实故障长这样：BMCLAPI 被限流后，`/version/<id>/json` 仍然回 200，但内容是
//! 一张 nginx 的错误页。旧代码直接 `into_json`，用户看到的是 JSON 解析失败的英文
//! 报错，根本不知道是镜像在挡人。这里把那几件事钉住：
//!
//! 1. 明显不是 JSON 的响应 → 换下一个地址，最终能成功；
//! 2. 内容错误不重试同一地址，报错里带上是**谁**回的不是 JSON；
//! 3. 体积离谱的响应直接拒收（镜像可能把整个 jar 当 JSON 灌过来）。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use kaleid_core_plugins::RemoteClient;

/// 一个只会照本宣科的极小 HTTP/1.1 服务：`replies[i]` 是第 i 次请求的回应，
/// 用完之后一直重复最后一个。
struct StubServer {
    base: String,
    hits: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl StubServer {
    fn start(replies: Vec<(u16, &'static str, String)>) -> StubServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        listener.set_nonblocking(false).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let thread = std::thread::spawn(move || {
            let mut served = 0usize;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                counter.fetch_add(1, Ordering::SeqCst);
                // 读完请求头就行，测试里不关心 body。
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let reply = replies
                    .get(served)
                    .or_else(|| replies.last())
                    .cloned()
                    .unwrap_or((500, "text/plain", "no reply".to_string()));
                served += 1;
                let (status, ctype, body) = reply;
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        StubServer {
            base,
            hits,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        // 监听线程靠进程退出收尾：这里只保证不 join 卡住测试。
        if let Some(handle) = self.thread.take() {
            drop(handle);
        }
    }
}

fn valid_index() -> String {
    r#"{"objects":{"icons/icon_16x16.png":{"hash":"a","size":1}}}"#.to_string()
}

#[test]
fn html_error_page_falls_back_to_the_next_address() {
    // 镜像被限流：200 但给的是 HTML。
    let mirror = StubServer::start(vec![(
        200,
        "text/html; charset=utf-8",
        "<html><body>429 Too Many Requests</body></html>".to_string(),
    )]);
    let official = StubServer::start(vec![(200, "application/json", valid_index())]);

    let client = RemoteClient::new();
    let urls = vec![mirror.url("/version/1.21.4/json"), official.url("/1.21.4.json")];
    let value = client
        .fetch_verified_json(&urls, "1.21.4 的资源索引")
        .expect("官方地址能给出 JSON，就应当成功");
    assert!(value.get("objects").is_some());
    assert!(mirror.hits.load(Ordering::SeqCst) >= 1, "应当先试过镜像");
}

#[test]
fn content_errors_are_not_retried_on_the_same_address() {
    let mirror = StubServer::start(vec![(
        200,
        "text/html",
        "<html><body>403 Forbidden</body></html>".to_string(),
    )]);
    let official = StubServer::start(vec![(
        200,
        "text/html",
        "<html><body>502 Bad Gateway</body></html>".to_string(),
    )]);

    let client = RemoteClient::new();
    let urls = vec![mirror.url("/version/1.21.4/json"), official.url("/1.21.4.json")];
    let error = client
        .fetch_verified_json(&urls, "1.21.4 的资源索引")
        .expect_err("两个地址都不是 JSON，必须报错");
    let text = error.to_string();
    assert!(
        text.contains("不是 JSON"),
        "错误信息要说清是内容不对，而不是解析器报错：{text}"
    );
    assert!(
        text.contains("1.21.4 的资源索引"),
        "错误信息要说明在下载什么：{text}"
    );
    assert!(text.contains("403 Forbidden") || text.contains("html"), "带上片段方便排查：{text}");
    assert_eq!(mirror.hits.load(Ordering::SeqCst), 1, "内容错误不该重试同一个地址");
    assert_eq!(official.hits.load(Ordering::SeqCst), 1, "第二个地址也应当只试一次");
    assert!(
        text.contains("127.0.0.1"),
        "要报出是哪台主机回的东西：{text}"
    );
    assert!(
        text.contains("/1.21.4.json") || text.contains("/version/1.21.4/json"),
        "要报出具体 URL，而不是只有主机：{text}"
    );
}

#[test]
fn rejects_a_json_file_that_is_too_large() {
    // 16 MiB 上限：镜像如果把一个大文件当 JSON 塞过来，宁可报错也别吃内存。
    let big = format!(r#"{{"objects":{{"pad":"{}"}}}}"#, "x".repeat(17 * 1024 * 1024));
    let server = StubServer::start(vec![(200, "application/json", big)]);
    let client = RemoteClient::new();
    let error = client
        .fetch_verified_json(&[server.url("/big.json")], "巨型索引")
        .expect_err("超过上限要报错");
    let text = error.to_string();
    assert!(text.contains("过大") || text.contains("上限"), "{text}");
}

#[test]
fn plain_json_without_charset_still_parses() {
    let server = StubServer::start(vec![(200, "application/json", valid_index())]);
    let client = RemoteClient::new();
    let value = client
        .fetch_verified_json(&[server.url("/ok.json")], "索引")
        .expect("正常 JSON 必须能读");
    assert!(value.get("objects").is_some());
}

#[test]
fn malformed_json_falls_back_without_refetching_bad_content() {
    let bad = StubServer::start(vec![(200, "application/json", r#"{"objects":"#.into())]);
    let good = StubServer::start(vec![(200, "application/json", valid_index())]);
    let value = RemoteClient::new()
        .fetch_verified_json(&[bad.url("/truncated.json"), good.url("/healthy.json")], "索引")
        .expect("malformed JSON must not suppress a healthy later source");
    assert!(value["objects"]["icons/icon_16x16.png"].is_object());
    assert_eq!(bad.hits.load(Ordering::SeqCst), 1);
    assert_eq!(good.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn all_malformed_json_candidates_report_each_failure_once() {
    let first = StubServer::start(vec![(200, "application/json", "{ truncated".into())]);
    let second = StubServer::start(vec![(200, "application/json", "[ broken".into())]);
    let first_url = first.url("/first.json");
    let second_url = second.url("/second.json");
    let error = RemoteClient::new()
        .fetch_verified_json(&[first_url.clone(), second_url.clone()], "索引")
        .expect_err("every malformed candidate must fail").to_string();
    assert!(error.contains(&first_url), "{error}");
    assert!(error.contains(&second_url), "{error}");
    assert_eq!(first.hits.load(Ordering::SeqCst), 1);
    assert_eq!(second.hits.load(Ordering::SeqCst), 1);
}

/// 真机：26.3 的索引必须能从 BMCLAPI 的 `/v1/packages/...` 拿到。
/// `cargo test -p kaleid-core-plugins --test remote_metadata real_26_3 -- --ignored --nocapture`
#[test]
#[ignore = "联网，验证 26.3 资源索引映射"]
fn real_26_3_asset_index_uses_packages_path() {
    let client = RemoteClient::new();
    let version: serde_json::Value = client
        .get_json("https://piston-meta.mojang.com/v1/packages/5df1795e16eea19d75b923d66ecca9ce183353c5/26.3.json")
        .expect("26.3 版本 JSON");
    let index = version.get("assetIndex").expect("assetIndex");
    let url = index.get("url").and_then(|v| v.as_str()).expect("url");
    let mapped = kaleid_core_plugins::mirrors::bmclapi_for_official(url)
        .expect("piston-meta 必须能映射");
    assert!(mapped.contains("/v1/packages/"), "{mapped}");
    let value = client
        .fetch_verified_json(&[mapped.clone()], "资源索引 34")
        .expect("BMCLAPI 的 packages 路径");
    assert!(value.get("objects").and_then(|v| v.as_object()).is_some(), "{mapped}");
    eprintln!("index ok: {mapped}");
}
