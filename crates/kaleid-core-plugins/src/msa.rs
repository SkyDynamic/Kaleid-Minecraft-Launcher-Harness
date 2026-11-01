//! 微软账号登录：OAuth2 设备码 → Xbox Live → XSTS → Minecraft。
//!
//! # 为什么是设备码
//!
//! 启动器是**公共客户端**（没有 client_secret，也不该有）：授权码流程要么需要
//! 内嵌浏览器窗口去抢回调端口，要么需要 `client_secret`。设备码流程只要求
//! "把一串短码显示给用户，让他在浏览器里输入"，没有回调端口、没有密钥、
//! 也不需要 WebView 里再套一个登录页——HMCL / PCL2 走的也是这条路。
//!
//! # 五步链
//!
//! 1. `devicecode` 拿 `user_code` → 用户浏览器确认 → 轮询 `token` 拿微软令牌
//! 2. `user.auth.xboxlive.com` 换 Xbox Live 令牌（`RpsTicket=d=<msa access>`）
//! 3. `xsts.auth.xboxlive.com` 换 XSTS 令牌（`RelyingParty=rp://api.minecraftservices.com/`）
//! 4. `login_with_xbox` 用 `XBL3.0 x=<uhs>;<xsts>` 换 Minecraft 访问令牌
//! 5. `entitlements/mcstore` 查所有权 + `minecraft/profile` 拿名字/UUID/皮肤
//!
//! 第 3 步的 `XErr` 是**唯一**能解释"为什么登不上"的地方（没买游戏、地区不支持、
//! 未成年需要家长同意……），所以那里把每个错误码都翻译成人话，而不是抛一句
//! "XSTS 失败"。
//!
//! # 令牌存哪
//!
//! 刷新令牌用 Windows DPAPI 加密后落盘（`<数据目录>/msa-tokens/<uuid>.bin`），
//! 只有**当前 Windows 用户**能解开；`accounts.json` 里永远只有名字、UUID 和皮肤，
//! 有专门的测试扫这个文件确认没有 `eyJ` / `refresh_token` 之类的痕迹。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use kaleid_api::{
    Account, AccountKind, AccountServiceApi, ApiError, ApiResult, DeviceCodeInfo, MsaAccountState,
    MsaAuthApi, MsaConfig, MsaPoll,
};

use crate::accounts::Accounts;

const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
/// `offline_access` 是为了拿到刷新令牌：没有它，一小时后就得让用户重登。
const SCOPE: &str = "XboxLive.signin offline_access";

const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_STORE_URL: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

/// 访问令牌提前 60 秒算过期，避免"刚好在两秒后失效"的边界。
const TOKEN_SKEW_SECS: u64 = 60;
/// 轮询间隔下限：微软要求至少 5 秒，我们固定按 **10 秒**一次问（更保守一点：
/// 慢几秒知道结果没什么损失，但被限流/`slow_down` 的概率更低，也更省电）。
const MIN_INTERVAL: u64 = 5;
/// `slow_down` 时每次加多少秒（OAuth2 设备码规范的做法）。
const SLOW_DOWN_STEP: u64 = 5;

// ---------------------------------------------------------------------------
// client_id：编译期从 build.env 注入
// ---------------------------------------------------------------------------

/// 编译期注入的 client_id（`build.env` 的 `KALEID_MSA_CLIENT_ID`）。
///
/// 用 `option_env!` 而不是 `env!`：缺这个键时编译**不**失败（否则任何人都克隆下来
/// 都编不过），但登录入口会明确告诉用户该往哪儿填。也正因为它是编译期的，
/// 换 client_id 必须重新编译——这一条写在 build.env 的注释里。
pub fn client_id() -> Option<&'static str> {
    let raw = option_env!("KALEID_MSA_CLIENT_ID")?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "REPLACE_ME" || trimmed.len() < 8 {
        None
    } else {
        Some(trimmed)
    }
}

/// 没配 client_id 时给用户看的句子（界面与错误都复用它）。
pub const MISSING_CLIENT_ID_HINT: &str =
    "还没配置微软应用的 client_id：请在仓库根目录 build.env 里填 KALEID_MSA_CLIENT_ID=…（Azure 应用注册里的「应用程序(客户端) ID」），然后重新编译。";

// ---------------------------------------------------------------------------
// 纯解析：全部离线可测
// ---------------------------------------------------------------------------

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

fn u64_at(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| v.as_u64())
}

/// 解析 `devicecode` 响应。
pub fn parse_device_code(value: &Value, started_at: u64) -> ApiResult<DeviceCodeInfo> {
    let user_code = str_at(value, "user_code")
        .ok_or_else(|| ApiError::Format("devicecode 响应缺少 user_code".into()))?;
    // 微软两个字段名都在用：新版是 verification_uri，老文档写 verification_url。
    let verification_uri = str_at(value, "verification_uri")
        .or_else(|| str_at(value, "verification_url"))
        .unwrap_or("https://www.microsoft.com/link");
    let expires_in = u64_at(value, "expires_in").unwrap_or(900);
    let interval = u64_at(value, "interval")
        .unwrap_or(MIN_INTERVAL)
        .max(MIN_INTERVAL);
    let message = str_at(value, "message")
        .map(str::to_string)
        .unwrap_or_else(|| format!("在浏览器打开 {verification_uri} 并输入 {user_code}"));
    Ok(DeviceCodeInfo {
        user_code: user_code.to_string(),
        verification_uri: verification_uri.to_string(),
        expires_at: started_at + expires_in,
        interval,
        message,
    })
}

/// 设备码申请失败时的人话。
///
/// `AADSTS700016` / `AADSTS50059` 是"应用注册本身不适合这条路"，不是网络问题，
/// 用户改代码也没用——必须去门户改应用的"支持的帐户类型"与"公共客户端流"。
/// 这两个码在调试时最容易被当成"启动器写错了"，所以这里直接把该点的开关写出来。
pub fn describe_device_code_error(status: u16, value: &Value) -> String {
    let error = str_at(value, "error").unwrap_or("unknown_error");
    let description = str_at(value, "error_description").unwrap_or("");
    let code = value
        .get("error_codes")
        .and_then(|v| v.as_array())
        .and_then(|codes| codes.first())
        .and_then(|v| v.as_i64());
    let hint = match code {
        Some(700016) | Some(50059) => {
            "\n\n两种可能，都不是代码问题：\n\
             · client_id 打错了（它必须是 Azure 应用注册里的「应用程序(客户端) ID」）；\n\
             · 那个应用注册还不能用于个人 Microsoft 帐户。\n\
             改法：Azure 门户 →「应用注册」→ 该应用 →「身份验证」\n\
             1) 「支持的帐户类型」改成「任何组织目录中的帐户和个人 Microsoft 帐户」；\n\
             2) 打开「允许公共客户端流」（Allow public client flows）。\n\
             改完不需要换 client_id，重启登录器即可。"
        }
        _ => "",
    };
    format!("微软拒绝生成设备码（HTTP {status}）：{error} {description}{hint}")
}

/// Minecraft 服务把原因放在 `errorMessage`，Xbox 服务放在 `Message`，两套都要认。
fn error_text(value: &Value) -> Option<&str> {
    str_at(value, "errorMessage")
        .or_else(|| str_at(value, "Message"))
        .or_else(|| str_at(value, "error"))
        .filter(|text| !text.trim().is_empty())
}

/// 自建 Azure 应用专用的申请入口。
///
/// `api.minecraftservices.com` 不是"有 XSTS 就能进"：它只接受**申请过 Minecraft API
/// 权限**的应用。自建应用即使 OAuth → Xbox Live → XSTS 全部成功，`login_with_xbox`
/// 也会回 `403 {"errorMessage":"Invalid app registration…"}`。
/// 这条最容易被当成"令牌拼错了/代码写错了"，所以把申请入口直接写进错误里。
const APP_REGISTRATION_HINT: &str =
    "这不是令牌的问题：Mojang 只允许「申请过 Minecraft API 权限」的 Azure 应用调用 \
     api.minecraftservices.com。自建应用请先提交 https://aka.ms/mce-reviewappid 申请，\
     审批通过前 login_with_xbox 一律返回 403。";

/// 把一个非 2xx 的 Minecraft/Xbox 响应翻成人话。
pub fn describe_mc_error(url: &str, status: u16, value: &Value) -> String {
    let detail = error_text(value).unwrap_or("服务器没有返回原因");
    if detail
        .to_ascii_lowercase()
        .contains("invalid app registration")
    {
        return format!("{url} 返回 HTTP {status}：{APP_REGISTRATION_HINT}\n官方原文：{detail}");
    }
    format!("{url} 返回 HTTP {status}：{detail}")
}

/// 刷新失败的两种性质：要不要让用户重新登录，完全是两件事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenFailure {
    /// 微软明确说这个刷新令牌不能用了（`invalid_grant` / `interaction_required`…）。
    /// 只能重新走一次设备码登录。
    Relogin(String),
    /// 网络、5xx、限流之类：令牌本身还是好的，过一会儿再试就行。
    Retryable(String),
}

impl TokenFailure {
    pub fn message(&self) -> &str {
        match self {
            TokenFailure::Relogin(message) | TokenFailure::Retryable(message) => message,
        }
    }
}

/// 刷新时微软回的 `error` 里，哪些意味着"这个刷新令牌彻底废了"。
///
/// 这些错误码必须清掉本地令牌：留着它下次还会继续失败，用户会以为是启动器坏了。
fn is_relogin_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant"
            | "interaction_required"
            | "consent_required"
            | "unauthorized_client"
            | "bad_verification_code"
    )
}

/// 把令牌端点的失败响应分类。`status`/`value` 与 `classify_token` 同一套语义。
fn classify_refresh_failure(value: &Value, status: u16) -> TokenFailure {
    let code = str_at(value, "error").unwrap_or_default();
    let description = str_at(value, "error_description").unwrap_or("");
    if is_relogin_error(code) {
        return TokenFailure::Relogin(format!(
            "微软不再认可这个账号的登录状态（{code}）。请重新登录。{description}"
        ));
    }
    if code.is_empty() {
        return TokenFailure::Retryable(format!(
            "刷新登录令牌失败（HTTP {status}）：微软没有返回原因"
        ));
    }
    TokenFailure::Retryable(format!("刷新登录令牌失败（{code}）：{description}"))
}

/// 一次 poll 换来的一组微软令牌。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

/// `token` 端点的三种正常结局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenPoll {
    Pending { interval: u64 },
    SlowDown { interval: u64 },
    Authorized(TokenSet),
}

/// 解析 `token` 响应。`status` 与 `body` 一起看：微软用 HTTP 400 + `error` 字段
/// 表达"还没确认"，这不是异常。
pub fn classify_token(value: &Value, status: u16, interval: u64) -> ApiResult<TokenPoll> {
    if let Some(err) = str_at(value, "error") {
        let description = str_at(value, "error_description").unwrap_or("");
        return match err {
            "authorization_pending" => Ok(TokenPoll::Pending { interval }),
            "slow_down" => Ok(TokenPoll::SlowDown {
                interval: interval.saturating_add(SLOW_DOWN_STEP),
            }),
            "authorization_declined" => Err(ApiError::Invalid(
                "登录被取消：你在浏览器里点了拒绝".into(),
            )),
            "expired_token" | "code_expired" => {
                Err(ApiError::Invalid("设备码已过期，请重新发起登录".into()))
            }
            "bad_verification_code" => {
                Err(ApiError::Invalid("设备码无效，请重新发起登录".into()))
            }
            "invalid_client" => Err(ApiError::Invalid(format!(
                "微软不认这个 client_id（{description}）：检查 build.env 里的 KALEID_MSA_CLIENT_ID，\
                 并确认那个应用注册是「公共客户端」且允许「个人 Microsoft 帐户」"
            ))),
            other => Err(ApiError::Network(format!(
                "微软登录失败：{other} {description}"
            ))),
        };
    }

    let access_token = str_at(value, "access_token").ok_or_else(|| {
        ApiError::Network(format!("微软登录响应看不懂（HTTP {status}）：{value}"))
    })?;
    Ok(TokenPoll::Authorized(TokenSet {
        access_token: access_token.to_string(),
        refresh_token: str_at(value, "refresh_token").map(str::to_string),
        expires_in: u64_at(value, "expires_in").unwrap_or(86_400),
    }))
}

/// Xbox Live 认证：返回 `(uhs, token)`。
pub fn parse_xbox_auth(value: &Value) -> ApiResult<(String, String)> {
    let token = str_at(value, "Token")
        .ok_or_else(|| ApiError::Format("Xbox Live 认证响应缺少 Token".into()))?;
    let uhs = value
        .get("DisplayClaims")
        .and_then(|v| v.get("xui"))
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .and_then(|v| str_at(v, "uhs"))
        .ok_or_else(|| ApiError::Format("Xbox Live 认证响应缺少 user hash".into()))?;
    Ok((uhs.to_string(), token.to_string()))
}

/// XSTS 的 `XErr` 翻译。这些数字是"用户到底缺什么"的全部信息来源。
pub fn xsts_error_message(code: i64) -> &'static str {
    match code {
        2_148_916_233 => "这个微软账号还没有 Xbox 档案：先在 xbox.com 登录一次创建档案，再回来重试",
        2_148_916_235 => "这个账号所在的国家/地区不支持 Xbox Live",
        2_148_916_236 | 2_148_916_237 => "这个账号需要在 xbox.com 完成成人验证（年龄信息缺失）",
        2_148_916_238 => "这个账号是未成年人：需要家长把它加进 Microsoft 家庭组才能真正登录",
        2_148_916_227 => "这个账号已被 Xbox Live 封禁",
        _ => "Xbox Live 授权失败（未知错误码）",
    }
}

/// XSTS 授权：返回 `(uhs, xsts token)`。
pub fn parse_xsts(value: &Value) -> ApiResult<(String, String)> {
    // 失败时 HTTP 401 + `{XErr: 2148916233}`，必须先看这个再找 Token。
    if let Some(code) = value.get("XErr").and_then(|v| v.as_i64()) {
        return Err(ApiError::Invalid(format!(
            "{}（XErr {code}）",
            xsts_error_message(code)
        )));
    }
    let token =
        str_at(value, "Token").ok_or_else(|| ApiError::Format("XSTS 响应缺少 Token".into()))?;
    let uhs = value
        .get("DisplayClaims")
        .and_then(|v| v.get("xui"))
        .and_then(|v| v.as_array())
        .and_then(|items| items.first())
        .and_then(|v| str_at(v, "uhs"))
        .ok_or_else(|| ApiError::Format("XSTS 响应缺少 user hash".into()))?;
    Ok((uhs.to_string(), token.to_string()))
}

/// Minecraft 访问令牌。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinecraftToken {
    pub access_token: String,
    pub expires_in: u64,
}

pub fn parse_mc_login(value: &Value) -> ApiResult<MinecraftToken> {
    let access_token = str_at(value, "access_token")
        .ok_or_else(|| ApiError::Format("Minecraft 登录响应缺少 access_token".into()))?;
    Ok(MinecraftToken {
        access_token: access_token.to_string(),
        expires_in: u64_at(value, "expires_in").unwrap_or(86_400),
    })
}

/// 所有权检查：`items` 为空就是"这个账号没买游戏"。
pub fn parse_entitlements(value: &Value) -> bool {
    value
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| !items.is_empty())
        .unwrap_or(false)
}

/// 玩家档案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McProfile {
    pub uuid: String,
    pub name: String,
    pub skin_url: Option<String>,
    /// `default` / `slim`
    pub skin_model: String,
}

pub fn parse_profile(value: &Value) -> ApiResult<McProfile> {
    let uuid = str_at(value, "id").ok_or_else(|| ApiError::Format("玩家档案缺少 id".into()))?;
    let name = str_at(value, "name").ok_or_else(|| ApiError::Format("玩家档案缺少 name".into()))?;
    // 有多个皮肤时只认 ACTIVE 那个；没有就留空，界面回落到内置皮肤。
    let skin = value
        .get("skins")
        .and_then(|v| v.as_array())
        .and_then(|skins| {
            skins
                .iter()
                .find(|s| str_at(s, "state") == Some("ACTIVE"))
                .or_else(|| skins.first())
        });
    let skin_url = skin
        .and_then(|s| str_at(s, "url"))
        .map(str::to_string)
        .filter(|u| u.starts_with("http"));
    let skin_model = match skin.and_then(|s| str_at(s, "variant")) {
        Some("SLIM") => "slim",
        _ => "default",
    };
    Ok(McProfile {
        uuid: uuid.to_string(),
        name: name.to_string(),
        skin_url,
        skin_model: skin_model.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 令牌存储：DPAPI 加密后落盘
// ---------------------------------------------------------------------------

/// 一个账号存下来的令牌（整体加密后才落盘）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredToken {
    /// 微软的刷新令牌。**这才是"登录状态"的本体**：Minecraft 访问令牌只有 24
    /// 小时，刷新令牌能撑几个月，而且每次续期都会换一个新的。
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    /// Minecraft 访问令牌的到期时间（Unix 秒）。
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// 上次成功续期的时间（Unix 秒）。用来避免短时间内反复打微软的令牌端点。
    #[serde(default)]
    pub last_refresh_at: Option<u64>,
    /// 微软已经明确拒绝了这个刷新令牌：再试也没用，必须重新登录。
    #[serde(default)]
    pub relogin_required: Option<bool>,
}

impl StoredToken {
    /// Minecraft 访问令牌还能直接用吗。
    pub fn fresh(&self, now: u64) -> bool {
        match (self.access_token.as_deref(), self.expires_at) {
            (Some(token), Some(exp)) => !token.is_empty() && exp > now + TOKEN_SKEW_SECS,
            _ => false,
        }
    }

    /// 还能靠刷新令牌续上吗。
    pub fn can_renew(&self) -> bool {
        !self.relogin_required.unwrap_or(false)
            && self
                .refresh_token
                .as_deref()
                .map(|token| !token.is_empty())
                .unwrap_or(false)
    }

    /// 这个账号的登录状态还能用吗（访问令牌没过期，或者能续）。
    ///
    /// 旧实现只看访问令牌，于是登录满 24 小时界面就显示"登录已过期"——明明刷新
    /// 令牌还好好的，点一下启动就能自动续。这就是"一天就失效"的来源。
    pub fn usable(&self, now: u64) -> bool {
        self.fresh(now) || self.can_renew()
    }
}

/// 令牌仓库（测试里换成内存实现，不需要碰 DPAPI）。
pub trait TokenStore: Send + Sync {
    fn save(&self, account_uuid: &str, token: &StoredToken) -> ApiResult<()>;
    fn load(&self, account_uuid: &str) -> ApiResult<Option<StoredToken>>;
    fn remove(&self, account_uuid: &str) -> ApiResult<()>;
}

/// 内存实现（测试与"临时登录"用）。
#[derive(Default)]
pub struct MemoryTokenStore {
    slots: Mutex<HashMap<String, StoredToken>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenStore for MemoryTokenStore {
    fn save(&self, account_uuid: &str, token: &StoredToken) -> ApiResult<()> {
        // 与文件实现共用同一套键规范化：UUID 带不带连字符、大小写如何都不影响命中。
        let key = sanitize_key(account_uuid)?;
        self.slots.lock().unwrap().insert(key, token.clone());
        Ok(())
    }

    fn load(&self, account_uuid: &str) -> ApiResult<Option<StoredToken>> {
        let key = sanitize_key(account_uuid)?;
        Ok(self.slots.lock().unwrap().get(&key).cloned())
    }

    fn remove(&self, account_uuid: &str) -> ApiResult<()> {
        let key = sanitize_key(account_uuid)?;
        self.slots.lock().unwrap().remove(&key);
        Ok(())
    }
}

/// `<数据目录>/msa-tokens/<uuid>.bin`，内容用 DPAPI 加密。
pub struct FileTokenStore {
    dir: PathBuf,
}

impl FileTokenStore {
    pub fn new(data_root: &std::path::Path) -> Self {
        FileTokenStore {
            dir: data_root.join("msa-tokens"),
        }
    }

    fn path_for(&self, account_uuid: &str) -> ApiResult<PathBuf> {
        let key = sanitize_key(account_uuid)?;
        Ok(self.dir.join(format!("{key}.bin")))
    }
}

/// 只允许 UUID 形态的键：这个值会变成文件名，绝不能让调用方拼路径。
fn sanitize_key(account_uuid: &str) -> ApiResult<String> {
    let key: String = account_uuid
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    if key.len() != 32 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::invalid(format!(
            "账号 UUID 不合法（不能用作令牌文件名）：{account_uuid}"
        )));
    }
    Ok(key)
}

impl TokenStore for FileTokenStore {
    fn save(&self, account_uuid: &str, token: &StoredToken) -> ApiResult<()> {
        let path = self.path_for(account_uuid)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let plain = serde_json::to_vec(token)?;
        let sealed = protect(&plain)?;
        std::fs::write(&path, sealed)?;
        Ok(())
    }

    fn load(&self, account_uuid: &str) -> ApiResult<Option<StoredToken>> {
        let path = self.path_for(account_uuid)?;
        let sealed = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ApiError::io(e)),
        };
        let plain = unprotect(&sealed)?;
        let token = serde_json::from_slice(&plain)?;
        Ok(Some(token))
    }

    fn remove(&self, account_uuid: &str) -> ApiResult<()> {
        let path = self.path_for(account_uuid)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ApiError::io(e)),
        }
    }
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> ApiResult<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        );
        if ok == 0 {
            return Err(ApiError::io(format!(
                "DPAPI 加密失败：{}",
                std::io::Error::last_os_error()
            )));
        }
        let sealed = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut core::ffi::c_void);
        Ok(sealed)
    }
}

#[cfg(windows)]
fn unprotect(sealed: &[u8]) -> ApiResult<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: sealed.len() as u32,
            pbData: sealed.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        );
        if ok == 0 {
            return Err(ApiError::io(format!(
                "DPAPI 解密失败（令牌可能属于另一个 Windows 账号，或文件被改动过）：{}",
                std::io::Error::last_os_error()
            )));
        }
        let plain = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut core::ffi::c_void);
        Ok(plain)
    }
}

#[cfg(not(windows))]
fn protect(_plain: &[u8]) -> ApiResult<Vec<u8>> {
    Err(ApiError::Unavailable(
        "令牌加密目前只用 Windows DPAPI 实现".into(),
    ))
}

#[cfg(not(windows))]
fn unprotect(_sealed: &[u8]) -> ApiResult<Vec<u8>> {
    Err(ApiError::Unavailable(
        "令牌加密目前只用 Windows DPAPI 实现".into(),
    ))
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

/// 正在进行中的设备码登录。
#[derive(Debug, Clone)]
struct PendingLogin {
    device_code: String,
    interval: u64,
    expires_at: u64,
}

pub struct MsaAuth {
    client_id: Option<String>,
    accounts: Arc<Accounts>,
    store: Arc<dyn TokenStore>,
    agent: ureq::Agent,
    pending: Mutex<Option<PendingLogin>>,
}

impl MsaAuth {
    pub fn new(client_id: Option<&str>, accounts: Arc<Accounts>) -> Self {
        Self::with_store(client_id, accounts, Arc::new(MemoryTokenStore::new()))
    }

    pub fn with_store(
        client_id: Option<&str>,
        accounts: Arc<Accounts>,
        store: Arc<dyn TokenStore>,
    ) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(30))
            .user_agent("KaleidLauncher/0.1")
            .build();
        MsaAuth {
            client_id: client_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            accounts,
            store,
            agent,
            pending: Mutex::new(None),
        }
    }

    /// 用编译期注入的 client_id 构造（插件走这条）。
    pub fn from_build(accounts: Arc<Accounts>, store: Arc<dyn TokenStore>) -> Self {
        Self::with_store(client_id(), accounts, store)
    }

    fn require_client_id(&self) -> ApiResult<&str> {
        self.client_id
            .as_deref()
            .ok_or_else(|| ApiError::Unavailable(MISSING_CLIENT_ID_HINT.to_string()))
    }

    /// 微软令牌 + Xbox + XSTS + Minecraft，一次走完。
    fn minecraft_from_msa(&self, msa_access: &str) -> ApiResult<(MinecraftToken, McProfile)> {
        let (_uhs, xbl) = self.xbox_authenticate(msa_access)?;
        let (xsts_uhs, xsts) = self.xsts_authorize(&xbl)?;
        let mc = self.login_with_xbox(&xsts_uhs, &xsts)?;
        // 所有权要自己查：没买的账号也能拿到令牌，进服才失败，那样错误太远了。
        let store = self.get_bearer(MC_STORE_URL, &mc.access_token)?;
        if !parse_entitlements(&store) {
            return Err(ApiError::invalid(
                "这个微软账号没有 Minecraft: Java Edition 的所有权（也没通过 Xbox Game Pass 认证）",
            ));
        }
        let profile = parse_profile(&self.get_bearer(MC_PROFILE_URL, &mc.access_token)?)?;
        Ok((mc, profile))
    }

    fn xbox_authenticate(&self, msa_access: &str) -> ApiResult<(String, String)> {
        let body = json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                // RPS 票据格式就是 `d=<微软访问令牌>`，别加 Bearer。
                "RpsTicket": format!("d={msa_access}")
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        });
        let value = self.post_json(XBL_URL, &body)?;
        parse_xbox_auth(&value)
    }

    fn xsts_authorize(&self, xbl_token: &str) -> ApiResult<(String, String)> {
        let body = json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        });
        let value = self.post_json(XSTS_URL, &body)?;
        parse_xsts(&value)
    }

    fn login_with_xbox(&self, uhs: &str, xsts: &str) -> ApiResult<MinecraftToken> {
        let body = json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts}") });
        let value = self.post_json(MC_LOGIN_URL, &body)?;
        parse_mc_login(&value)
    }

    /// 登录成功后：登记账号 + 保存令牌（顺序保证不会出现"令牌存了但账号没有"）。
    fn commit(
        &self,
        set: &TokenSet,
        mc: &MinecraftToken,
        profile: &McProfile,
    ) -> ApiResult<Account> {
        let account = self.accounts.upsert_msa_profile(
            &profile.name,
            &profile.uuid,
            &profile.skin_model,
            profile.skin_url.as_deref(),
        )?;
        self.store.save(
            &profile.uuid,
            &StoredToken {
                refresh_token: set.refresh_token.clone(),
                access_token: Some(mc.access_token.clone()),
                expires_at: Some(now_secs() + mc.expires_in),
                last_refresh_at: Some(now_secs()),
                relogin_required: None,
            },
        )?;
        Ok(account)
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> ApiResult<(u16, Value)> {
        match self.agent.post(url).send_form(form) {
            Ok(response) => {
                let status = response.status();
                let value = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
                Ok((status, value))
            }
            // ureq 把 4xx/5xx 变成 Err(Status)，但设备码流程恰恰靠 400 的 body 说话。
            Err(ureq::Error::Status(code, response)) => {
                let value = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
                Ok((code, value))
            }
            Err(e) => Err(ApiError::Network(format!("{url}: {e}"))),
        }
    }

    fn post_json(&self, url: &str, body: &Value) -> ApiResult<Value> {
        let (status, value) = match self
            .agent
            .post(url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(body.clone())
        {
            Ok(response) => {
                let status = response.status();
                let value = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
                (status, value)
            }
            Err(ureq::Error::Status(code, response)) => {
                let value = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
                (code, value)
            }
            Err(e) => return Err(ApiError::Network(format!("{url}: {e}"))),
        };
        if (200..300).contains(&status) {
            return Ok(value);
        }
        // XSTS 的错误码藏在 body 里，交给 parse_xsts 翻译成人话。
        if value.get("XErr").is_some() {
            return Ok(value);
        }
        Err(ApiError::Network(describe_mc_error(url, status, &value)))
    }

    fn get_bearer(&self, url: &str, token: &str) -> ApiResult<Value> {
        let bearer = format!("Bearer {token}");
        match self.agent.get(url).set("Authorization", &bearer).call() {
            Ok(response) => response
                .into_json::<Value>()
                .map_err(|e| ApiError::Network(format!("{url}: {e}"))),
            Err(ureq::Error::Status(code, response)) => {
                let value = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
                // 只有 401 才是"令牌过期"；403 往往是应用权限，别把人往重新登录上带。
                let tail = if code == 401 {
                    "（登录可能已过期，重新登录一次）"
                } else {
                    ""
                };
                Err(ApiError::Network(format!(
                    "{}{tail}",
                    describe_mc_error(url, code, &value)
                )))
            }
            Err(e) => Err(ApiError::Network(format!("{url}: {e}"))),
        }
    }

    fn account_by_id(&self, account_id: &str) -> ApiResult<Account> {
        self.accounts
            .list()?
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or_else(|| ApiError::not_found(format!("账号 {account_id}")))
    }
}

impl MsaAuthApi for MsaAuth {
    fn config(&self) -> MsaConfig {
        MsaConfig {
            configured: self.client_id.is_some(),
            hint: if self.client_id.is_some() {
                None
            } else {
                Some(MISSING_CLIENT_ID_HINT.to_string())
            },
        }
    }

    fn start(&self) -> ApiResult<DeviceCodeInfo> {
        let client_id = self.require_client_id()?;
        let (status, value) = self.post_form(
            DEVICE_CODE_URL,
            &[("client_id", client_id), ("scope", SCOPE)],
        )?;
        if !(200..300).contains(&status) {
            return Err(ApiError::Network(describe_device_code_error(
                status, &value,
            )));
        }
        let info = parse_device_code(&value, now_secs())?;
        let device_code = str_at(&value, "device_code")
            .ok_or_else(|| ApiError::Format("devicecode 响应缺少 device_code".into()))?
            .to_string();
        *self.pending.lock().unwrap() = Some(PendingLogin {
            device_code,
            interval: info.interval,
            expires_at: info.expires_at,
        });
        Ok(info)
    }

    fn poll(&self) -> ApiResult<MsaPoll> {
        let client_id = self.require_client_id()?.to_string();
        let pending = {
            let guard = self.pending.lock().unwrap();
            guard
                .clone()
                .ok_or_else(|| ApiError::invalid("还没有发起登录：先点「添加正版账号」"))?
        };
        let now = now_secs();
        if now >= pending.expires_at {
            *self.pending.lock().unwrap() = None;
            return Err(ApiError::invalid("设备码已过期，请重新发起登录"));
        }

        let (status, value) = self.post_form(
            TOKEN_URL,
            &[
                ("client_id", &client_id),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &pending.device_code),
            ],
        )?;

        // 终态错误（用户拒绝 / 码过期 / client_id 无效）之后这次登录就废了：
        // 清掉 pending，免得同一个死掉的设备码被反复重试。
        let classified = match classify_token(&value, status, pending.interval) {
            Ok(classified) => classified,
            Err(err) => {
                *self.pending.lock().unwrap() = None;
                return Err(err);
            }
        };

        match classified {
            TokenPoll::Pending { interval } => {
                if let Some(slot) = self.pending.lock().unwrap().as_mut() {
                    slot.interval = interval;
                }
                Ok(MsaPoll::Pending {
                    interval,
                    expires_in: pending.expires_at.saturating_sub(now),
                })
            }
            TokenPoll::SlowDown { interval } => {
                if let Some(slot) = self.pending.lock().unwrap().as_mut() {
                    slot.interval = interval;
                }
                Ok(MsaPoll::SlowDown { interval })
            }
            TokenPoll::Authorized(set) => {
                let (mc, profile) = self.minecraft_from_msa(&set.access_token)?;
                let account = self.commit(&set, &mc, &profile)?;
                *self.pending.lock().unwrap() = None;
                Ok(MsaPoll::Done {
                    account,
                    expires_in: mc.expires_in,
                })
            }
        }
    }

    fn cancel(&self) -> ApiResult<()> {
        *self.pending.lock().unwrap() = None;
        Ok(())
    }

    fn states(&self) -> ApiResult<Vec<MsaAccountState>> {
        let now = now_secs();
        let mut out = Vec::new();
        for account in self
            .accounts
            .list()?
            .into_iter()
            .filter(|a| a.kind == AccountKind::Msa)
        {
            let stored = self.store.load(&account.uuid)?;
            out.push(MsaAccountState {
                account_id: account.id,
                has_refresh_token: stored
                    .as_ref()
                    .and_then(|s| s.refresh_token.as_deref())
                    .map(|t| !t.is_empty())
                    .unwrap_or(false),
                expires_at: stored.as_ref().and_then(|s| s.expires_at),
                // 能续期就算"可用"：访问令牌过期只是需要自动续一次，不等于掉登录。
                usable: stored.as_ref().map(|s| s.usable(now)).unwrap_or(false),
                relogin_required: stored
                    .as_ref()
                    .map(|s| s.relogin_required.unwrap_or(false))
                    .unwrap_or(false),
            });
        }
        Ok(out)
    }

    fn sign_out(&self, account_id: &str) -> ApiResult<()> {
        let account = self.account_by_id(account_id)?;
        if account.kind == AccountKind::Msa {
            self.store.remove(&account.uuid)?;
        }
        Ok(())
    }

    fn access_token(&self, account_id: &str) -> ApiResult<Option<String>> {
        let account = self.account_by_id(account_id)?;
        if account.kind != AccountKind::Msa {
            return Ok(None);
        }
        let now = now_secs();
        let stored = self.store.load(&account.uuid)?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        if stored.fresh(now) {
            return Ok(stored.access_token.clone());
        }
        if stored.relogin_required.unwrap_or(false) || !stored.can_renew() {
            return Ok(None);
        }
        match self.renew(&account.uuid, &stored) {
            Ok(token) => Ok(Some(token)),
            // 微软明确说刷新令牌不能用了：当作"需要重新登录"（宿主/界面会说清楚），
            // 而不是抛一个看起来像网络故障的错误。
            Err(TokenFailure::Relogin(_)) => Ok(None),
            Err(TokenFailure::Retryable(message)) => Err(ApiError::Network(message)),
        }
    }
}

impl MsaAuth {
    /// 用刷新令牌换一个新的 Minecraft 访问令牌。
    ///
    /// 关键顺序（这就是"隔天登录就没了"的根因）：**微软一给出新的刷新令牌就立刻
    /// 落盘**，然后才去打通 XBL → XSTS → Minecraft。旧实现把落盘放在最后，中间
    /// 任何一步网络抖动都会让"已经作废的旧刷新令牌 + 没拿到的新刷新令牌"一起丢掉，
    /// 用户从此只能重新登录。
    fn renew(&self, uuid: &str, stored: &StoredToken) -> Result<String, TokenFailure> {
        let refresh_token = stored
            .refresh_token
            .clone()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| TokenFailure::Relogin("账号没有可用的刷新令牌，请重新登录".into()))?;
        let client_id = self
            .client_id
            .as_deref()
            .ok_or_else(|| TokenFailure::Retryable(MISSING_CLIENT_ID_HINT.to_string()))?;
        let (status, value) = self
            .post_form(
                TOKEN_URL,
                &[
                    ("client_id", client_id),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh_token),
                    ("scope", SCOPE),
                ],
            )
            .map_err(|err| TokenFailure::Retryable(format!("刷新登录令牌失败：{err}")))?;

        let set = match classify_token(&value, status, MIN_INTERVAL) {
            Ok(TokenPoll::Authorized(set)) => set,
            Ok(TokenPoll::Pending { .. }) | Ok(TokenPoll::SlowDown { .. }) => {
                return Err(TokenFailure::Retryable(
                    "微软没有立刻受理这次令牌刷新，稍后会自动重试".into(),
                ))
            }
            Err(err) => {
                // `classify_token` 把认不出的 `error` 归到 Network；刷新场景里那些
                // 恰恰包括 invalid_grant 这类"必须重新登录"的终态，所以这里按错误码
                // 重新分类，终态的还要把本地令牌标成"需要重新登录"。
                let failure = classify_refresh_failure(&value, status);
                if matches!(failure, TokenFailure::Relogin(_)) {
                    self.mark_relogin(uuid);
                    return Err(failure);
                }
                if str_at(&value, "error").is_none() {
                    // 微软没给错误码（真正的网络/解析失败）：保留原来的中文说明。
                    return Err(TokenFailure::Retryable(format!("刷新登录令牌失败：{err}")));
                }
                return Err(failure);
            }
        };
        // 第一步先落盘：新的刷新令牌已经到手，旧的很可能已经作废。
        let rotated = set
            .refresh_token
            .clone()
            .unwrap_or_else(|| refresh_token.clone());
        self.save_refreshed(uuid, stored, &rotated, None, Some(now_secs()))
            .map_err(|err| TokenFailure::Retryable(format!("保存令牌失败：{err}")))?;

        let (mc, _profile) = match self.minecraft_from_msa(&set.access_token) {
            Ok(pair) => pair,
            // 令牌已经存好了，下次直接重试即可——不算"登录失效"。
            Err(err) => {
                return Err(TokenFailure::Retryable(format!(
                    "登录状态已续期，但换取 Minecraft 令牌失败：{err}"
                )))
            }
        };
        self.save_refreshed(
            uuid,
            stored,
            &rotated,
            Some((mc.access_token.clone(), now_secs() + mc.expires_in)),
            Some(now_secs()),
        )
        .map_err(|err| TokenFailure::Retryable(format!("保存令牌失败：{err}")))?;
        Ok(mc.access_token)
    }

    /// 微软拒绝了这个刷新令牌：把本地这份标记成"需要重新登录"。
    ///
    /// 不直接删文件：账号条目还在，界面需要知道"是过期了，不是从没登录过"。
    fn mark_relogin(&self, uuid: &str) {
        if let Ok(Some(mut stored)) = self.store.load(uuid) {
            stored.relogin_required = Some(true);
            let _ = self.store.save(uuid, &stored);
        }
    }

    /// 保存续期结果。`mc` 为 `None` 时只更新刷新令牌与续期时间。
    fn save_refreshed(
        &self,
        uuid: &str,
        previous: &StoredToken,
        refresh_token: &str,
        mc: Option<(String, u64)>,
        refreshed_at: Option<u64>,
    ) -> ApiResult<()> {
        let (access_token, expires_at) = match mc {
            Some((token, expires_at)) => (Some(token), Some(expires_at)),
            None => (previous.access_token.clone(), previous.expires_at),
        };
        self.store.save(
            uuid,
            &StoredToken {
                refresh_token: Some(refresh_token.to_string()),
                access_token,
                expires_at,
                last_refresh_at: refreshed_at.or(previous.last_refresh_at),
                relogin_required: None,
            },
        )
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 测试（全部离线：解析、错误翻译、令牌存储；不碰微软接口）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;

    fn fixture_device_code() -> Value {
        json!({
            "user_code": "K7Q2WP4XL",
            "device_code": "DEVICECODE-abc",
            "verification_uri": "https://www.microsoft.com/link",
            "expires_in": 900,
            "interval": 5,
            "message": "若要登录，请使用 Web 浏览器打开页面 https://www.microsoft.com/link 并输入代码 K7Q2WP4XL 以进行身份验证。"
        })
    }

    #[test]
    fn device_code_parses_and_computes_expiry() {
        let info = parse_device_code(&fixture_device_code(), 1_000).unwrap();
        assert_eq!(info.user_code, "K7Q2WP4XL");
        // 微软说 5 秒，我们按 10 秒问：慢一点更稳，也不会触发 slow_down
        assert_eq!(info.interval, MIN_INTERVAL);
        assert_eq!(info.expires_at, 1_900);
        assert!(info.message.contains("K7Q2WP4XL"));
    }

    #[test]
    fn device_code_falls_back_to_old_field_name_and_min_interval() {
        let value = json!({
            "user_code": "ABC",
            "verification_url": "https://microsoft.com/devicelogin",
            "expires_in": 600,
            "interval": 0
        });
        let info = parse_device_code(&value, 0).unwrap();
        assert_eq!(info.verification_uri, "https://microsoft.com/devicelogin");
        // 间隔 0 会被拉到规范下限，否则界面会把微软打到限流
        assert_eq!(info.interval, MIN_INTERVAL);
    }

    #[test]
    fn minecraft_403_invalid_app_registration_points_at_the_application_form() {
        // 真实响应：自建应用没申请 Minecraft API 权限时就是这么回的
        let value = json!({
            "path": "/authentication/login_with_xbox",
            "errorMessage": "Invalid app registration, see https://aka.ms/AppRegInfo for more information"
        });
        let message = describe_mc_error(MC_LOGIN_URL, 403, &value);
        assert!(message.contains("403"), "{message}");
        assert!(message.contains("mce-reviewappid"), "{message}");
        assert!(message.contains("api.minecraftservices.com"), "{message}");

        // 普通的 4xx 不要乱指路，原样把服务器的话带出来
        let ok = describe_mc_error(MC_STORE_URL, 401, &json!({ "error": "UNAUTHORIZED" }));
        assert!(!ok.contains("mce-reviewappid"), "{ok}");
        assert!(ok.contains("UNAUTHORIZED"), "{ok}");

        // 连原因都没有的响应也得说清是哪一步、什么状态码
        let bare = describe_mc_error(MC_PROFILE_URL, 500, &json!({}));
        assert!(bare.contains("500"), "{bare}");
        assert!(bare.contains(MC_PROFILE_URL), "{bare}");
    }

    #[test]
    fn token_poll_classifies_pending_slow_down_and_authorized() {
        let pending = json!({"error": "authorization_pending", "error_description": "AADSTS70016"});
        assert_eq!(
            classify_token(&pending, 400, 5).unwrap(),
            TokenPoll::Pending { interval: 5 }
        );

        let slow = json!({"error": "slow_down"});
        assert_eq!(
            classify_token(&slow, 400, 5).unwrap(),
            TokenPoll::SlowDown { interval: 10 }
        );

        let ok = json!({
            "access_token": "eyJ.msa",
            "refresh_token": "M.R3_BAY.xxx",
            "expires_in": 3600
        });
        let TokenPoll::Authorized(set) = classify_token(&ok, 200, 5).unwrap() else {
            panic!("应当判定为授权成功");
        };
        assert_eq!(set.access_token, "eyJ.msa");
        assert_eq!(set.refresh_token.as_deref(), Some("M.R3_BAY.xxx"));
    }

    #[test]
    fn device_code_error_explains_the_portal_switch_for_wrong_tenant_apps() {
        // 这条是真实抓到的响应：client_id 属于单租户应用时，`/consumers/` 就是这么回的。
        let value = json!({
            "error": "unauthorized_client",
            "error_description": "AADSTS700016: Application with identifier '1d223e4c-…' was not found in the directory '9188040d-6c67-4c5b-b112-36a304b66dad'.",
            "error_codes": [700016]
        });
        let message = describe_device_code_error(400, &value);
        assert!(message.contains("AADSTS700016"), "{message}");
        assert!(message.contains("个人 Microsoft 帐户"), "{message}");
        assert!(message.contains("公共客户端流"), "{message}");
        // 打错一位 GUID 与"单租户应用"是同一个错误码，两种可能都要说
        assert!(message.contains("打错"), "{message}");

        // `/common/` 对单租户应用回的是 50059，同样要给出同一个指引
        let tenant = json!({
            "error": "invalid_request",
            "error_description": "AADSTS50059: No tenant-identifying information found",
            "error_codes": [50059]
        });
        assert!(describe_device_code_error(400, &tenant).contains("公共客户端流"));

        // 其它错误别乱指路：真正的网络/服务故障要按原样报告
        let other = describe_device_code_error(503, &json!({ "error": "temporarily_unavailable" }));
        assert!(!other.contains("公共客户端流"), "{other}");
        assert!(other.contains("503"), "{other}");
    }

    #[test]
    fn token_poll_turns_terminal_errors_into_readable_sentences() {
        let declined = json!({"error": "authorization_declined"});
        assert!(classify_token(&declined, 400, 5)
            .unwrap_err()
            .to_string()
            .contains("取消"));

        let expired = json!({"error": "expired_token"});
        assert!(classify_token(&expired, 400, 5)
            .unwrap_err()
            .to_string()
            .contains("过期"));

        // client_id 配错时要说清楚该改哪里，而不是只丢一个 invalid_client
        let bad_client = json!({"error": "invalid_client", "error_description": "AADSTS700016"});
        let message = classify_token(&bad_client, 401, 5).unwrap_err().to_string();
        assert!(message.contains("KALEID_MSA_CLIENT_ID"), "{message}");
    }

    #[test]
    fn xsts_errors_explain_what_the_user_is_missing() {
        for (code, needle) in [
            (2_148_916_233i64, "Xbox 档案"),
            (2_148_916_235, "国家/地区"),
            (2_148_916_238, "家庭组"),
        ] {
            let message = parse_xsts(&json!({ "XErr": code }))
                .unwrap_err()
                .to_string();
            assert!(message.contains(needle), "{code}: {message}");
            assert!(message.contains(&code.to_string()), "要带上原始错误码");
        }
        // 未知码也要保留数字，方便用户去搜
        let unknown = parse_xsts(&json!({ "XErr": 123456 }))
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("123456"));
    }

    #[test]
    fn xbox_and_xsts_share_the_same_claim_shape() {
        let value = json!({
            "Token": "jwt-token",
            "DisplayClaims": { "xui": [{ "uhs": "1234567890123456" }] }
        });
        assert_eq!(
            parse_xbox_auth(&value).unwrap(),
            ("1234567890123456".to_string(), "jwt-token".to_string())
        );
        assert_eq!(
            parse_xsts(&value).unwrap(),
            ("1234567890123456".to_string(), "jwt-token".to_string())
        );
        assert!(
            parse_xbox_auth(&json!({ "Token": "x" })).is_err(),
            "缺 uhs 要报错"
        );
    }

    #[test]
    fn entitlements_must_contain_at_least_one_item() {
        assert!(parse_entitlements(
            &json!({"items": [{"name": "product_minecraft"}]})
        ));
        assert!(!parse_entitlements(&json!({"items": []})));
        assert!(!parse_entitlements(&json!({})));
    }

    #[test]
    fn profile_reads_active_skin_and_variant() {
        let value = json!({
            "id": "069a79f444e94726a5befca90e38aaff",
            "name": "SkyDy",
            "skins": [
                {"state": "INACTIVE", "url": "https://textures.minecraft.net/texture/old", "variant": "CLASSIC"},
                {"state": "ACTIVE", "url": "https://textures.minecraft.net/texture/new", "variant": "SLIM"}
            ]
        });
        let profile = parse_profile(&value).unwrap();
        assert_eq!(profile.name, "SkyDy");
        assert_eq!(profile.skin_model, "slim");
        assert_eq!(
            profile.skin_url.as_deref(),
            Some("https://textures.minecraft.net/texture/new")
        );
        // 没有皮肤时留空，界面回落到内置史蒂夫
        let bare = parse_profile(&json!({"id": "a", "name": "B"})).unwrap();
        assert!(bare.skin_url.is_none());
        assert_eq!(bare.skin_model, "default");
    }

    #[test]
    fn memory_store_roundtrips_and_removes() {
        let store = MemoryTokenStore::new();
        let token = StoredToken {
            refresh_token: Some("refresh".into()),
            access_token: Some("access".into()),
            expires_at: Some(now_secs() + 3600),
            ..Default::default()
        };
        store
            .save("069A79F4-44E9-4726-A5BE-FCA90E38AAFF", &token)
            .unwrap();
        // UUID 大小写不该影响命中
        let loaded = store
            .load("069a79f444e94726a5befca90e38aaff")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh"));
        store.remove("069a79f444e94726a5befca90e38aaff").unwrap();
        assert!(store
            .load("069a79f444e94726a5befca90e38aaff")
            .unwrap()
            .is_none());
    }

    #[test]
    fn stored_token_uses_skew_when_deciding_freshness() {
        let now = 1_000_000;
        let almost = StoredToken {
            refresh_token: None,
            access_token: Some("a".into()),
            expires_at: Some(now + TOKEN_SKEW_SECS - 1),
            ..Default::default()
        };
        assert!(!almost.fresh(now), "快过期就该刷新，别等启动那一刻掉线");
        assert!(almost.fresh(now - 120));
    }

    #[test]
    fn token_store_rejects_path_like_keys() {
        let store = MemoryTokenStore::new();
        let _ = store;
        for bad in ["../../evil", "not-a-uuid", "069a79f444e94726a5befca90e38aa"] {
            assert!(sanitize_key(bad).is_err(), "应拒绝 {bad:?}");
        }
        assert!(sanitize_key("069A79F4-44E9-4726-A5BE-FCA90E38AAFF").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_store_encrypts_on_disk_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let uuid = "069a79f4-44e9-4726-a5be-fca90e38aaff";
        let store = FileTokenStore::new(dir.path());
        let token = StoredToken {
            refresh_token: Some("M.R3_BAY.secret-refresh".into()),
            access_token: Some("mc-access".into()),
            expires_at: Some(now_secs() + 3600),
            ..Default::default()
        };
        store.save(uuid, &token).unwrap();

        let path = dir
            .path()
            .join("msa-tokens")
            .join("069a79f444e94726a5befca90e38aaff.bin");
        let raw = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            !text.contains("secret-refresh"),
            "落盘内容必须是密文，不能出现明文刷新令牌"
        );

        // 重新打开（模拟下次启动）仍能读回
        let reopened = FileTokenStore::new(dir.path());
        let loaded = reopened.load(uuid).unwrap().unwrap();
        assert_eq!(
            loaded.refresh_token.as_deref(),
            Some("M.R3_BAY.secret-refresh")
        );

        reopened.remove(uuid).unwrap();
        assert!(reopened.load(uuid).unwrap().is_none());
        assert!(!path.exists());
    }

    fn auth_with(client_id: Option<&str>) -> (MsaAuth, Arc<Accounts>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Arc::new(Accounts::open(dir.path()).unwrap());
        let auth = MsaAuth::new(client_id, accounts.clone());
        (auth, accounts, dir)
    }

    #[test]
    fn missing_client_id_says_exactly_where_to_put_it() {
        let (auth, _accounts, _dir) = auth_with(None);
        let config = auth.config();
        assert!(!config.configured);
        assert!(config
            .hint
            .as_deref()
            .unwrap()
            .contains("KALEID_MSA_CLIENT_ID"));

        let message = auth.start().unwrap_err().to_string();
        assert!(message.contains("build.env"), "{message}");
        assert!(message.contains("client_id"), "{message}");
    }

    /// 「一天就失效」的根因：`usable` 只认 Minecraft 访问令牌的 24 小时有效期。
    /// 刷新令牌还好好的，界面却已经写"登录已过期"。
    #[test]
    fn an_expired_access_token_does_not_mean_logged_out() {
        let now = 1_000_000;
        let after_a_day = StoredToken {
            refresh_token: Some("M.R3_BAY.refresh".into()),
            access_token: Some("mc-access".into()),
            expires_at: Some(now - 60),
            ..Default::default()
        };
        assert!(!after_a_day.fresh(now), "访问令牌确实过期了");
        assert!(after_a_day.can_renew(), "但刷新令牌还在");
        assert!(
            after_a_day.usable(now),
            "能自动续期就该算有效，否则界面会谎报掉登录"
        );

        // 刷新令牌被微软作废之后，才算真的需要重新登录。
        let dead = StoredToken {
            relogin_required: Some(true),
            ..after_a_day.clone()
        };
        assert!(!dead.can_renew());
        assert!(!dead.usable(now));
    }

    /// 微软明确拒绝刷新令牌（invalid_grant）与"网络不通"必须分开：前者只能重新
    /// 登录，后者等一会儿就好。旧代码把两者都当成 `ApiError::Invalid` 一并报
    /// "请重新登录"，网络抖一下就把用户的登录状态判了死刑。
    #[test]
    fn refresh_failures_are_split_into_relogin_and_retryable() {
        let invalid_grant = json!({
            "error": "invalid_grant",
            "error_description": "AADSTS70008: The refresh token has expired."
        });
        match classify_refresh_failure(&invalid_grant, 400) {
            TokenFailure::Relogin(message) => {
                assert!(message.contains("重新登录"), "{message}");
            }
            other => panic!("invalid_grant 必须是终态：{other:?}"),
        }

        let interaction = json!({"error": "interaction_required"});
        assert!(matches!(
            classify_refresh_failure(&interaction, 400),
            TokenFailure::Relogin(_)
        ));

        let server_error = json!({"error": "temporarily_unavailable"});
        assert!(matches!(
            classify_refresh_failure(&server_error, 503),
            TokenFailure::Retryable(_)
        ));

        // 微软什么都没说（网关挡了、返回了一段 HTML）：只能当临时故障。
        assert!(matches!(
            classify_refresh_failure(&json!({}), 502),
            TokenFailure::Retryable(_)
        ));
    }

    /// 续期时**先**把微软刚给的刷新令牌落盘，再去打通 XBL → XSTS → Minecraft。
    ///
    /// 旧实现反过来：等到 Minecraft 令牌拿到之后才存。中间任何一步网络抖动，用户
    /// 就会同时失去"已经作废的旧刷新令牌"和"没存下来的新刷新令牌"，从此只能重新
    /// 登录——这就是"隔天登录就没了"最常见的触发方式。
    #[test]
    fn rotated_refresh_token_is_saved_before_the_xbox_chain_runs() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Arc::new(Accounts::open(dir.path()).unwrap());
        let memory = Arc::new(MemoryTokenStore::new());
        let auth = MsaAuth::with_store(Some("0000-client-id"), accounts, memory.clone());
        let uuid = "069a79f444e94726a5befca90e38aaff";
        let previous = StoredToken {
            refresh_token: Some("old-refresh".into()),
            access_token: Some("old-mc".into()),
            expires_at: Some(1_000),
            ..Default::default()
        };
        memory.save(uuid, &previous).unwrap();

        auth.save_refreshed(uuid, &previous, "rotated-refresh", None, Some(2_000))
            .unwrap();

        let stored = memory.load(uuid).unwrap().unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("rotated-refresh"));
        assert_eq!(stored.last_refresh_at, Some(2_000));
        // Minecraft 令牌还没换到：旧值原地留着，别清空成"没登录过"。
        assert_eq!(stored.access_token.as_deref(), Some("old-mc"));
        assert_eq!(stored.expires_at, Some(1_000));
        assert!(!stored.relogin_required.unwrap_or(false));
    }

    /// 微软作废刷新令牌时只做标记，不删账号记录：界面得能说清"是过期了"。
    #[test]
    fn rejected_refresh_token_is_marked_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Arc::new(Accounts::open(dir.path()).unwrap());
        let memory = Arc::new(MemoryTokenStore::new());
        let auth = MsaAuth::with_store(Some("0000-client-id"), accounts, memory.clone());
        let uuid = "069a79f444e94726a5befca90e38aaff";
        memory
            .save(
                uuid,
                &StoredToken {
                    refresh_token: Some("dead".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        auth.mark_relogin(uuid);
        let stored = memory.load(uuid).unwrap().unwrap();
        assert_eq!(stored.relogin_required, Some(true));
        assert_eq!(stored.refresh_token.as_deref(), Some("dead"));
        assert!(!stored.can_renew(), "标记之后不该再拿它去撞微软");
    }

    #[test]
    fn poll_before_start_is_an_actionable_error() {        let (auth, _accounts, _dir) = auth_with(Some("0000-client-id"));
        let message = auth.poll().unwrap_err().to_string();
        assert!(message.contains("添加正版账号"), "{message}");
        // cancel 永远成功：界面关闭弹窗时不该因为"本来就没在登录"报错
        assert!(auth.cancel().is_ok());
    }

    #[test]
    fn sign_out_drops_the_saved_token_but_keeps_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Arc::new(Accounts::open(dir.path()).unwrap());
        let memory = Arc::new(MemoryTokenStore::new());
        let auth = MsaAuth::with_store(Some("0000-client-id"), accounts.clone(), memory.clone());
        let account = accounts
            .upsert_msa_profile(
                "SkyDy",
                "069a79f444e94726a5befca90e38aaff",
                "slim",
                Some("https://textures.minecraft.net/texture/x"),
            )
            .unwrap();

        // 没有令牌时退出登录也要成功（按钮不该报错）
        auth.sign_out(&account.id).unwrap();

        // 有令牌时会被清掉，但账号条目留着（删账号是 accounts_remove 的事）
        memory
            .save(
                "069a79f444e94726a5befca90e38aaff",
                &StoredToken {
                    refresh_token: Some("refresh".into()),
                    access_token: Some("access".into()),
                    expires_at: None,
                    ..Default::default()
                },
            )
            .unwrap();
        auth.sign_out(&account.id).unwrap();
        assert!(memory
            .load("069a79f444e94726a5befca90e38aaff")
            .unwrap()
            .is_none());
        assert_eq!(accounts.list().unwrap().len(), 1);
    }

    #[test]
    fn states_report_missing_tokens_per_account() {
        let dir = tempfile::tempdir().unwrap();
        let accounts = Arc::new(Accounts::open(dir.path()).unwrap());
        let auth = MsaAuth::new(Some("0000-client-id"), accounts.clone());
        let account = accounts
            .upsert_msa_profile("SkyDy", "069a79f444e94726a5befca90e38aaff", "default", None)
            .unwrap();

        let states = auth.states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].account_id, account.id);
        assert!(!states[0].has_refresh_token);
        assert!(!states[0].usable);
        assert!(states[0].expires_at.is_none());

        // 离线账号不出现在这里
        accounts.apply_offline_name("Player_123").unwrap();
        assert_eq!(auth.states().unwrap().len(), 1);
    }

    #[test]
    fn accounts_file_still_has_no_token_after_profile_upsert() {
        let (_auth, accounts, dir) = auth_with(Some("0000-client-id"));
        accounts
            .upsert_msa_profile(
                "SkyDy",
                "069a79f444e94726a5befca90e38aaff",
                "slim",
                Some("https://textures.minecraft.net/texture/x"),
            )
            .unwrap();
        let text = std::fs::read_to_string(paths::accounts_file(dir.path())).unwrap();
        for needle in ["eyJ", "refresh", "access_token", "M.R3_BAY"] {
            assert!(!text.contains(needle), "accounts.json 不得出现 {needle}");
        }
        // 皮肤直链倒是应该写进去了（界面靠它画头像）
        assert!(text.contains("textures.minecraft.net"));
    }

    #[test]
    fn client_id_from_build_is_a_uuid_shaped_guid() {
        // build.env 里没填时不算失败（本地可能删了这个键），填了就必须是 GUID。
        if let Some(id) = client_id() {
            assert_eq!(id.len(), 36, "client_id 应当是 GUID：{id}");
            assert!(id.chars().next().unwrap().is_ascii_hexdigit(), "{id}");
        }
    }
}
