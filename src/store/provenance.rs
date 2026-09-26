//! 交付账本（spec §9.3）。它只回答一个问题：**这个目标是不是 henv 放那儿的**。
//! 「是不是」决定了 `apply` 有没有资格动它——不变式二是「只碰账本里记着的条目」。

use crate::capability::Kind;
use crate::home::Home;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 账本格式版本。与 `manifest::SUPPORTED_VERSION` **分开**：清单格式与账本格式是
/// 两个独立演进面（清单变了不必动账本，反之亦然），共用一个常量会让一边被迫跟版。
pub const LEDGER_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<LedgerEntry>,
}

impl Default for Ledger {
    fn default() -> Self {
        Ledger {
            version: LEDGER_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// 目标 harness 名（`adapters::CLAUDE_CODE` 等三个之一）。
    pub harness: String,
    /// 能力种类。B1 只写 `Skill`；字段在是因为 §9.3 的账本要覆盖 B3 的 MCP 写入，
    /// 那时候补字段得升 version，不如现在就留好。
    pub kind: Kind,
    /// 能力名（skill 是目录名）。
    pub name: String,
    /// 交付到的绝对路径。
    pub path: PathBuf,
    /// 来源标识（§9.3）：B1 是 `agents-dir` 或 `local:<dir>`；B2/B3 再加
    /// `<repo>@<ref>@<sha>` 与 `bundle@<from>@<version>@<sha>`。
    /// 它**不参与比较**，是给人读的——所以同一个来源在不同机器上应当写出同一个串。
    pub source_id: String,
    /// 交付完成后在目标上重算的 `hash_dir` 值（不是解析源时的值，见 `apply`）。
    /// §9.4 的 staleness 判定与 §9.3 的「副本被手改」标注都读它。
    pub content_hash: String,
    /// 交付时间，Unix 秒。不用格式化时间串是为了不引日期库进依赖树。
    pub delivered_at_unix: u64,
}

impl Ledger {
    /// 读账本。**缺失 → 空账本，不是错误**（这台机器还没 apply 过）；
    /// **存在但解不开 → 报错**——与 adapter `read_state` 同一条失败隔离契约。
    pub fn load(home: &Home) -> anyhow::Result<Ledger> {
        let path = home.henv_provenance();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("cannot read ledger {}", path.display()))
                );
            }
        };
        let ledger: Ledger = serde_json::from_str(&text)
            .with_context(|| format!("ledger {} is not valid json", path.display()))?;
        anyhow::ensure!(
            ledger.version == LEDGER_VERSION,
            "ledger {} has version {}, this build only understands {LEDGER_VERSION}",
            path.display(),
            ledger.version
        );
        Ok(ledger)
    }

    /// 写账本。这是 `store` 里唯一的写点，且只被 `apply::execute` 调用。
    ///
    /// §9.7 的事务形态：同目录临时文件 → fsync → rename。**不落 `.bak`**——
    /// 账本是本工具自己的状态文件，被覆盖是它的正常工作方式，不是用户的手写配置。
    pub fn save(&self, home: &Home) -> anyhow::Result<()> {
        let path = home.henv_provenance();
        let dir = path
            .parent()
            .expect("provenance.json always has a parent directory");
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| (&a.harness, &a.kind, &a.name).cmp(&(&b.harness, &b.kind, &b.name)));
        let body = serde_json::to_vec_pretty(&Ledger {
            version: self.version,
            entries,
        })
        .context("cannot serialize ledger")?;

        let tmp = dir.join("provenance.json.henv-tmp");
        std::fs::write(&tmp, &body).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::File::open(&tmp)
            .with_context(|| format!("cannot reopen {}", tmp.display()))?
            .sync_all()
            .with_context(|| format!("cannot fsync {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }

    /// 按 `(harness, kind, name)` 找。**kind 不可省**：同名的一个 skill 与一个 mcp
    /// 落在不同的目标位置上（前者是 `~/.claude/skills/<n>`，后者是 `~/.claude.json`
    /// 里的一个键），账本把它们当成一条记录的话，后交付的那个会把前一个顶掉。
    pub fn find(&self, harness: &str, kind: Kind, name: &str) -> Option<&LedgerEntry> {
        self.entries
            .iter()
            .find(|e| e.harness == harness && e.kind == kind && e.name == name)
    }

    /// 按 `(harness, kind, name)` upsert。`apply` 交付成功后调用。
    pub fn record(&mut self, entry: LedgerEntry) {
        match self
            .entries
            .iter_mut()
            .find(|e| e.harness == entry.harness && e.kind == entry.kind && e.name == entry.name)
        {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }

    /// 摘掉一条记录，返回是否真的摘到了（§9.8：`--prune` 成功后调用）。
    ///
    /// 键与 [`Ledger::find`] / [`Ledger::record`] 完全一致：`(harness, kind, name)`。
    /// **摘不到不是错误**：重跑一次 `--prune` 是正常路径，返回 `false` 让调用方能区分
    /// 「摘掉了」与「本来就没了」，而不是把后者报成故障。
    pub fn remove(&mut self, harness: &str, kind: Kind, name: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| !(e.harness == harness && e.kind == kind && e.name == name));
        self.entries.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Kind;
    use crate::testutil::TempHome;

    fn entry(harness: &str, name: &str, hash: &str) -> LedgerEntry {
        entry_of(harness, Kind::Skill, name, hash)
    }

    fn entry_of(harness: &str, kind: Kind, name: &str, hash: &str) -> LedgerEntry {
        LedgerEntry {
            harness: harness.into(),
            kind,
            name: name.into(),
            path: PathBuf::from("/somewhere").join(name),
            source_id: "agents-dir".into(),
            content_hash: hash.into(),
            delivered_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn a_missing_ledger_is_empty_not_an_error() {
        // 这台机器还没 apply 过。缺失不是错误——与 adapter read_state 同一条契约。
        let t = TempHome::new();
        assert_eq!(Ledger::load(&t.home).unwrap(), Ledger::default());
        assert_eq!(Ledger::load(&t.home).unwrap().version, LEDGER_VERSION);
    }

    #[test]
    fn a_saved_ledger_round_trips() {
        let t = TempHome::new();
        let mut l = Ledger::default();
        l.record(entry("claude-code", "foo", "abc"));
        l.save(&t.home).unwrap();

        let back = Ledger::load(&t.home).unwrap();
        assert_eq!(back, l);
        assert_eq!(
            back.find("claude-code", Kind::Skill, "foo").unwrap().name,
            "foo"
        );
        assert!(back.find("claude-code", Kind::Skill, "bar").is_none());
        assert!(
            back.find("codex", Kind::Skill, "foo").is_none(),
            "同名不同家不能串"
        );
    }

    #[test]
    fn save_writes_entries_in_a_stable_order() {
        // 账本是人也会读的文件。按记录顺序落盘的话，两台机器上的同一份状态会
        // 写出两个不同的文件——纯粹是噪音。
        let t = TempHome::new();
        let mut l = Ledger::default();
        l.record(entry("codex", "zeta", "h3"));
        l.record(entry("claude-code", "beta", "h2"));
        l.record(entry("claude-code", "alpha", "h1"));
        l.save(&t.home).unwrap();

        let text = std::fs::read_to_string(t.home.henv_provenance()).unwrap();
        let pos = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle} 不在账本里"))
        };
        assert!(
            pos("\"alpha\"") < pos("\"beta\""),
            "同名 harness 内按名字排"
        );
        assert!(
            pos("\"claude-code\"") < pos("\"codex\""),
            "harness 之间按名字排"
        );
    }

    #[test]
    fn record_overwrites_the_same_harness_and_name() {
        let mut l = Ledger::default();
        l.record(entry("codex", "foo", "old"));
        l.record(entry("codex", "foo", "new"));
        assert_eq!(l.entries.len(), 1, "同 harness+name 是 upsert，不是追加");
        assert_eq!(
            l.find("codex", Kind::Skill, "foo").unwrap().content_hash,
            "new"
        );
    }

    /// §9.8：`--prune` 成功后摘账。键与 `find`/`record` 完全一致——
    /// 同名不同 kind、同名不同家都不许连坐。
    #[test]
    fn remove_drops_exactly_one_slot() {
        let mut l = Ledger::default();
        l.record(entry_of("claude-code", Kind::Skill, "foo", "h"));
        l.record(entry_of("claude-code", Kind::Mcp, "foo", "m"));
        l.record(entry_of("codex", Kind::Skill, "foo", "h2"));

        assert!(l.remove("claude-code", Kind::Skill, "foo"));
        assert!(l.find("claude-code", Kind::Skill, "foo").is_none());
        assert!(
            l.find("claude-code", Kind::Mcp, "foo").is_some(),
            "同名不同 kind 不能连坐"
        );
        assert!(
            l.find("codex", Kind::Skill, "foo").is_some(),
            "同名不同家不能连坐"
        );
        assert_eq!(l.entries.len(), 2);

        // 再删一次是 false，不是 panic——重跑 `--prune` 是正常路径。
        assert!(!l.remove("claude-code", Kind::Skill, "foo"));
        assert_eq!(l.entries.len(), 2);
    }

    /// B3a：同名的一个 skill 与一个 mcp 是**两件不同的东西**——
    /// `~/.claude/skills/foo` 与 `~/.claude.json` 里的 `foo` 没有任何关系。
    /// B1 的键只有 `(harness, name)`，mcp 一进来两边就会互相顶掉：
    /// 记了 mcp 之后 diff 会以为那个 skill 没交付过，转头报成 conflict。
    #[test]
    fn a_skill_and_an_mcp_with_the_same_name_occupy_different_slots() {
        let mut l = Ledger::default();
        l.record(entry_of("claude-code", Kind::Mcp, "foo", "mcp-hash"));
        l.record(entry_of("claude-code", Kind::Skill, "foo", "skill-hash"));

        assert_eq!(l.entries.len(), 2, "同名不同 kind 是两条记录，不是一条");
        assert_eq!(
            l.find("claude-code", Kind::Skill, "foo")
                .unwrap()
                .content_hash,
            "skill-hash"
        );
        assert_eq!(
            l.find("claude-code", Kind::Mcp, "foo")
                .unwrap()
                .content_hash,
            "mcp-hash"
        );

        // record 的覆盖也按三元组：同名 mcp 再记一次是替换那一条，不动 skill 那条
        l.record(entry_of("claude-code", Kind::Mcp, "foo", "mcp-v2"));
        assert_eq!(l.entries.len(), 2, "upsert 不该新增行");
        assert_eq!(
            l.find("claude-code", Kind::Mcp, "foo")
                .unwrap()
                .content_hash,
            "mcp-v2"
        );
        assert_eq!(
            l.find("claude-code", Kind::Skill, "foo")
                .unwrap()
                .content_hash,
            "skill-hash",
            "改 mcp 不能碰到同名 skill"
        );
    }

    #[test]
    fn a_malformed_ledger_is_an_error_not_an_empty_one() {
        // 「存在但解不开」必须报错。静默成空账本会让下一次 apply 把 henv 自己的
        // 交付物当成「别人建的」而拒绝更新，或者更糟：当成无主目录覆盖。
        let t = TempHome::new();
        t.write(".local/state/henv/provenance.json", "{ not json");
        let err = format!("{:#}", Ledger::load(&t.home).unwrap_err());
        assert!(err.contains("provenance.json"), "{err}");
    }

    #[test]
    fn a_ledger_from_a_future_version_is_refused() {
        let t = TempHome::new();
        t.write(
            ".local/state/henv/provenance.json",
            "{\"version\": 999, \"entries\": []}",
        );
        let err = format!("{:#}", Ledger::load(&t.home).unwrap_err());
        assert!(err.contains("999"), "{err}");
    }

    #[test]
    fn an_unknown_entry_field_is_refused() {
        // deny_unknown_fields：账本格式的演进必须走 version 升版，
        // 而不是让新字段在旧二进制眼里静默消失。
        let t = TempHome::new();
        t.write(
            ".local/state/henv/provenance.json",
            "{\"version\": 1, \"entries\": [{\"harness\":\"codex\",\"kind\":\"skill\",\
             \"name\":\"a\",\"path\":\"/p\",\"source_id\":\"s\",\"content_hash\":\"h\",\
             \"delivered_at_unix\":1,\"future\":true}]}",
        );
        assert!(Ledger::load(&t.home).is_err());
    }
}
