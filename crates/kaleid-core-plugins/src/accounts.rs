//! 账号（`AccountServiceApi`）。
//!
//! 三条硬规则：
//! 1. 账号**全局唯一一份**：所有文件夹、所有版本共用同一个 `activeAccountId`。
//! 2. 正版可以存**多个**；离线只保留**一个生效档案**（输入即改这一个，不堆历史）。
//! 3. 令牌**永不**进入 `accounts.json`。这里只存 `{id, kind, name, uuid, skinModel, lastUsed}`；
//!    正版令牌由账号插件写入 Windows 凭据管理器（DPAPI），本模块不接触。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use kaleid_api::events::AccountChanged;
use kaleid_api::{
    offline_uuid, validate_offline_name, Account, AccountKind, AccountServiceApi, ApiError,
    ApiResult,
};
use kaleid_kernel::Context;

use crate::jsonstore::{now_iso, read_json, write_json};
use crate::paths;

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedAccounts {
    #[serde(default)]
    active_account_id: Option<String>,
    #[serde(default)]
    accounts: Vec<Account>,
}

pub struct Accounts {
    file: PathBuf,
    state: Mutex<PersistedAccounts>,
    ctx: Option<Context>,
}

impl Accounts {
    pub fn open(data_root: &Path) -> ApiResult<Self> {
        let file = paths::accounts_file(data_root);
        let mut persisted: PersistedAccounts = read_json(&file)?.unwrap_or_default();
        // 选中项失效时回落到第一个
        if let Some(active) = persisted.active_account_id.clone() {
            if !persisted.accounts.iter().any(|a| a.id == active) {
                persisted.active_account_id = persisted.accounts.first().map(|a| a.id.clone());
            }
        } else {
            persisted.active_account_id = persisted.accounts.first().map(|a| a.id.clone());
        }
        Ok(Accounts {
            file,
            state: Mutex::new(persisted),
            ctx: None,
        })
    }

    pub fn empty_at(data_root: &Path) -> ApiResult<Self> {
        Ok(Accounts {
            file: paths::accounts_file(data_root),
            state: Mutex::new(PersistedAccounts::default()),
            ctx: None,
        })
    }

    pub fn with_context(mut self, ctx: Context) -> Self {
        self.ctx = Some(ctx);
        self
    }

    fn persist_locked(&self, state: &PersistedAccounts) -> ApiResult<()> {
        write_json(&self.file, state)
    }

    fn snapshot(&self) -> (Vec<Account>, Option<Account>) {
        let g = self.state.lock().unwrap();
        let active = g
            .active_account_id
            .as_ref()
            .and_then(|id| g.accounts.iter().find(|a| &a.id == id))
            .cloned();
        (g.accounts.clone(), active)
    }

    fn emit(&self) {
        if let Some(ctx) = &self.ctx {
            let (_, active) = self.snapshot();
            ctx.emit(AccountChanged { active });
        }
    }

    /// 正版账号由账号插件通过这里登记（不含任何令牌）。
    pub fn upsert_msa(&self, name: &str, uuid: &str, skin_model: &str) -> ApiResult<Account> {
        self.upsert_msa_profile(name, uuid, skin_model, None)
    }

    /// 带皮肤直链的登记：登录流程从 `minecraft/profile` 拿到皮肤后调这个。
    ///
    /// 皮肤不是令牌，可以落盘；`skin_url` 为 `None` 时保留上一次的值，
    /// 免得"这次 profile 没带皮肤"把界面上已经能画的头像抹掉。
    pub fn upsert_msa_profile(
        &self,
        name: &str,
        uuid: &str,
        skin_model: &str,
        skin_url: Option<&str>,
    ) -> ApiResult<Account> {
        let mut g = self.state.lock().unwrap();
        let existing = g
            .accounts
            .iter()
            .position(|a| a.kind == AccountKind::Msa && a.uuid.eq_ignore_ascii_case(uuid));
        let account = match existing {
            Some(idx) => {
                let a = &mut g.accounts[idx];
                a.name = name.to_string();
                a.skin_model = skin_model.to_string();
                if let Some(url) = skin_url {
                    a.skin_url = Some(url.to_string());
                }
                a.last_used = Some(now_iso());
                a.clone()
            }
            None => Account {
                id: uuid::Uuid::new_v4().to_string(),
                kind: AccountKind::Msa,
                name: name.to_string(),
                uuid: uuid.to_string(),
                skin_model: skin_model.to_string(),
                skin_url: skin_url.map(str::to_string),
                last_used: Some(now_iso()),
            },
        };
        if existing.is_none() {
            g.accounts.push(account.clone());
        }
        g.active_account_id = Some(account.id.clone());
        self.persist_locked(&g)?;
        drop(g);
        self.emit();
        Ok(account)
    }
}

impl AccountServiceApi for Accounts {
    fn list(&self) -> ApiResult<Vec<Account>> {
        let mut accounts = self.snapshot().0;
        // 最近用过的排前面
        accounts.sort_by(|a, b| {
            b.last_used
                .clone()
                .unwrap_or_default()
                .cmp(&a.last_used.clone().unwrap_or_default())
        });
        Ok(accounts)
    }

    fn active(&self) -> ApiResult<Option<Account>> {
        Ok(self.snapshot().1)
    }

    fn set_active(&self, id: &str) -> ApiResult<Account> {
        let account = {
            let mut g = self.state.lock().unwrap();
            let pos = g
                .accounts
                .iter()
                .position(|a| a.id == id)
                .ok_or_else(|| ApiError::not_found(format!("账号 {id}")))?;
            g.accounts[pos].last_used = Some(now_iso());
            let account = g.accounts[pos].clone();
            g.active_account_id = Some(account.id.clone());
            self.persist_locked(&g)?;
            account
        };
        self.emit();
        Ok(account)
    }

    /// **输入即生效**：校验通过就写入并设为当前账号；不通过则原样保留上一次的有效名字。
    fn apply_offline_name(&self, name: &str) -> ApiResult<Account> {
        validate_offline_name(name)?;
        let new_uuid = offline_uuid(name);

        let account = {
            let mut g = self.state.lock().unwrap();
            match g
                .accounts
                .iter()
                .position(|a| a.kind == AccountKind::Offline)
            {
                Some(idx) => {
                    // 只维护"一个离线档案"：改名就原地改写，不新建历史副本
                    let a = &mut g.accounts[idx];
                    a.name = name.to_string();
                    a.uuid = new_uuid;
                    a.last_used = Some(now_iso());
                    a.clone()
                }
                None => {
                    let a = Account {
                        id: uuid::Uuid::new_v4().to_string(),
                        kind: AccountKind::Offline,
                        name: name.to_string(),
                        uuid: new_uuid,
                        skin_model: "default".to_string(),
                        // 离线账号没有可取的皮肤，界面固定画史蒂夫。
                        skin_url: None,
                        last_used: Some(now_iso()),
                    };
                    g.accounts.push(a.clone());
                    a
                }
            }
        };

        {
            let mut g = self.state.lock().unwrap();
            g.active_account_id = Some(account.id.clone());
            self.persist_locked(&g)?;
        }
        self.emit();
        Ok(account)
    }

    fn remove(&self, id: &str) -> ApiResult<()> {
        {
            let mut g = self.state.lock().unwrap();
            let before = g.accounts.len();
            g.accounts.retain(|a| a.id != id);
            if g.accounts.len() == before {
                return Err(ApiError::not_found(format!("账号 {id}")));
            }
            if g.active_account_id.as_deref() == Some(id) {
                g.active_account_id = g.accounts.first().map(|a| a.id.clone());
            }
            self.persist_locked(&g)?;
        }
        self.emit();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_name_is_applied_live_and_reuses_one_profile() {
        let dir = tempfile::tempdir().unwrap();
        let acc = Accounts::empty_at(dir.path()).unwrap();

        let a = acc.apply_offline_name("Player_123").unwrap();
        assert_eq!(a.kind, AccountKind::Offline);
        assert_eq!(a.uuid, offline_uuid("Player_123"));
        assert_eq!(acc.active().unwrap().unwrap().name, "Player_123");

        // 改名：原地改写同一个档案，不新增
        let b = acc.apply_offline_name("Notch").unwrap();
        assert_eq!(b.id, a.id, "离线只维护一个生效档案");
        assert_eq!(b.uuid, offline_uuid("Notch"));
        assert_eq!(acc.list().unwrap().len(), 1);
    }

    #[test]
    fn invalid_offline_name_keeps_previous_account() {
        let dir = tempfile::tempdir().unwrap();
        let acc = Accounts::empty_at(dir.path()).unwrap();
        acc.apply_offline_name("Player_123").unwrap();

        for bad in ["", "玩家名", "bad name", "bad-name"] {
            assert!(acc.apply_offline_name(bad).is_err(), "应拒绝 {bad:?}");
            let active = acc.active().unwrap().unwrap();
            assert_eq!(active.name, "Player_123", "非法输入不得改动现状");
        }
        assert!(acc.apply_offline_name(&"a".repeat(17)).is_err());
    }

    #[test]
    fn multiple_msa_accounts_coexist_and_switch() {
        let dir = tempfile::tempdir().unwrap();
        let acc = Accounts::empty_at(dir.path()).unwrap();
        let a = acc
            .upsert_msa("SkyDy", "069a79f4-44e9-4726-a5be-fca90e38aaf5", "default")
            .unwrap();
        let b = acc
            .upsert_msa("Alex_2", "11111111-2222-3333-4444-555555555555", "slim")
            .unwrap();
        assert_eq!(acc.list().unwrap().len(), 2);
        // upsert 同一个 uuid 不应重复插入
        acc.upsert_msa("SkyDy", "069a79f4-44e9-4726-a5be-fca90e38aaf5", "default")
            .unwrap();
        assert_eq!(acc.list().unwrap().len(), 2);
        assert_eq!(acc.active().unwrap().unwrap().id, a.id);

        acc.set_active(&b.id).unwrap();
        assert_eq!(acc.active().unwrap().unwrap().id, b.id);
    }

    #[test]
    fn removing_active_account_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let acc = Accounts::empty_at(dir.path()).unwrap();
        let a = acc
            .upsert_msa("SkyDy", "069a79f4-44e9-4726-a5be-fca90e38aaf5", "default")
            .unwrap();
        let b = acc
            .upsert_msa("Alex_2", "11111111-2222-3333-4444-555555555555", "slim")
            .unwrap();
        acc.set_active(&b.id).unwrap();
        acc.remove(&b.id).unwrap();
        assert_eq!(acc.active().unwrap().unwrap().id, a.id);
        acc.remove(&a.id).unwrap();
        assert!(acc.active().unwrap().is_none());
    }

    #[test]
    fn accounts_file_never_contains_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let acc = Accounts::empty_at(dir.path()).unwrap();
        acc.upsert_msa("SkyDy", "069a79f4-44e9-4726-a5be-fca90e38aaf5", "default")
            .unwrap();
        acc.apply_offline_name("Player_123").unwrap();
        let text = std::fs::read_to_string(paths::accounts_file(dir.path())).unwrap();
        for needle in [
            "eyJ",
            "refreshToken",
            "refresh_token",
            "accessToken",
            "access_token",
        ] {
            assert!(
                !text.contains(needle),
                "accounts.json 不得出现 `{needle}`：\n{text}"
            );
        }
    }

    #[test]
    fn state_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let acc = Accounts::open(dir.path()).unwrap();
            acc.apply_offline_name("Player_123").unwrap();
            acc.upsert_msa("SkyDy", "069a79f4-44e9-4726-a5be-fca90e38aaf5", "default")
                .unwrap();
        }
        let acc = Accounts::open(dir.path()).unwrap();
        assert_eq!(acc.list().unwrap().len(), 2);
        assert_eq!(acc.active().unwrap().unwrap().name, "SkyDy");
    }
}
