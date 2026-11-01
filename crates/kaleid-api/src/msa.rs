//! 微软账号登录（OAuth2 设备码流程）。
//!
//! 契约只有"发起 / 轮询 / 取消 / 状态"四件事，**后端不睡眠、不轮询**：
//! `poll` 一次只做一次 HTTP 尝试并立刻返回，节奏由界面按 `interval` 自己掌握。
//! 这样点「取消」立刻就能停，也不会有一个后台线程在用户关掉界面之后还在打接口。
//!
//! 令牌**从不**出现在这些结构里（也不进 `accounts.json`）：这里只说
//! "有没有刷新令牌 / 访问令牌几点过期"，真正的令牌留在加密存储里。

use serde::{Deserialize, Serialize};

use crate::error::ApiResult;
use crate::model::Account;

/// 设备码：显示给用户，让他在浏览器/手机上输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodeInfo {
    /// 微软给用户输入的那串短码（形如 `ABCDEFGH`）。
    pub user_code: String,
    /// 用户该打开的页面（`https://www.microsoft.com/link`）。
    pub verification_uri: String,
    /// 设备码失效时间（Unix 秒）。界面用它显示倒计时。
    pub expires_at: u64,
    /// 建议轮询间隔（秒）。微软偶尔会要求放慢，那时 [`MsaPoll`] 会带新值。
    pub interval: u64,
    /// 微软原样的提示文案（中文/英文取决于请求头），界面可以整段展示。
    pub message: String,
}

/// 一次轮询的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum MsaPoll {
    /// 用户还没在浏览器里点完。
    Pending {
        /// 下次轮询间隔（秒）。
        interval: u64,
        /// 设备码还剩多少秒。
        #[serde(rename = "expiresIn")]
        expires_in: u64,
    },
    /// 微软明确要求放慢（`slow_down`）：按新间隔再来，别当错误处理。
    SlowDown {
        #[serde(rename = "interval")]
        interval: u64,
    },
    /// 成功，账号已经登记好并设为当前账号。
    Done {
        account: Account,
        /// Minecraft 访问令牌能活多久（秒）。过一会儿就要靠刷新令牌续。
        #[serde(rename = "expiresIn")]
        expires_in: u64,
    },
}

/// 单个正版账号的令牌状态（只有布尔值与时间，没有令牌本身）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsaAccountState {
    pub account_id: String,
    /// 有加密保存的刷新令牌 → 到期后还能静默续上。
    pub has_refresh_token: bool,
    /// 当前 Minecraft 访问令牌的到期时间（Unix 秒）。
    pub expires_at: Option<u64>,
    /// 现在能不能拿去启动。
    ///
    /// **不是**"访问令牌还没过期"：只要刷新令牌还在，过期了也能自动续，所以这里
    /// 表示"这次登录还能用"。真正需要用户动手的只有 [`Self::relogin_required`]。
    pub usable: bool,
    /// 微软已经明确说这个账号的刷新令牌不能用了（`invalid_grant` 之类）：
    /// 界面要提示"需要重新登录"，而不是"网络不好稍后再试"。
    pub relogin_required: bool,
}

/// 登录能力是否可用（`client_id` 是编译期注入的，可能没配）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsaConfig {
    pub configured: bool,
    /// 没配时给用户看的完整句子，说明该往哪儿填。
    pub hint: Option<String>,
}

/// 微软账号登录服务。
pub trait MsaAuthApi: Send + Sync {
    fn config(&self) -> MsaConfig;
    /// 发起设备码登录，返回要展示给用户的那串码。
    fn start(&self) -> ApiResult<DeviceCodeInfo>;
    /// 轮询一次。返回 [`MsaPoll::Pending`] 是正常状态，不是错误。
    fn poll(&self) -> ApiResult<MsaPoll>;
    /// 放弃这次登录（清掉设备码），已经登录过的账号不受影响。
    fn cancel(&self) -> ApiResult<()>;
    /// 每个正版账号的令牌状态。
    fn states(&self) -> ApiResult<Vec<MsaAccountState>>;
    /// 退出某个账号：删掉它保存的令牌（账号条目本身留给账号服务处理）。
    fn sign_out(&self, account_id: &str) -> ApiResult<()>;
    /// 取一个可用的 Minecraft 访问令牌，必要时用刷新令牌静默续期。
    ///
    /// 现在还没有真正的 spawn 流程，这个接口留给"启动前注入令牌"那一步。
    fn access_token(&self, account_id: &str) -> ApiResult<Option<String>>;
}
