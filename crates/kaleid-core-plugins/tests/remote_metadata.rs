//! 元数据抓取的验收：镜像返回"200 + 一段 HTML 错误页"时，不能只报一句
//! `expected value at line 1 column 1`。
//!
//! 真实故障长这样：BMCLAPI 被限流后，`/version/<id>/json` 仍然回 200，但内容是
//! 一张 nginx 的错误页。旧代码直接 `into_json`，用户看到的是 JSON 解析失败的英文
//! 报错，根本不知道是镜像在挡人。这里把那几件事钉住：
//!
//! 1. 明显不是 JSON 的响应 → 换下一个地址，最终能成功；
//! 2. 每个地址都试两轮才放弃，报错里带上是**谁**回的不是 JSON；
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
fn every_address_is_tried_twice_before_giving_up() {
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
    assert_eq!(mirror.hits.load(Ordering::SeqCst), 2, "第一个地址应当试两轮");
    assert_eq!(official.hits.load(Ordering::SeqCst), 2, "第二个地址也应当试两轮");
    assert!(
        text.contains("127.0.0.1"),
        "要报出是哪台主机回的东西：{text}"
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
