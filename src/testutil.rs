//! 测试专用：在 TempDir 里重建假 HOME。绝不触碰真实 ~。
#![allow(dead_code)]

use crate::adapters::HarnessAdapter;
use crate::apply::mcp::McpTarget;
use crate::capability::{
    CapId, Capability, Kind, McpTransport, McpValue, Origin, Spec, Support, Targets,
};
use crate::home::Home;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// 测试夹具：`&[("K", "${K}")]` → `BTreeMap<String, McpValue>`。
///
/// 按**清单门禁**那条规则解析（认得出的引用 → `Ref`，别的 → `Literal`），因为夹具要能写出
/// 两边：合法清单要的引用，以及手写配置里那种「像引用的字面量」（`Literal("${K}")`）。
///
/// **生产读路径不许用它**：哪一格是引用由**字段**决定（codex 的 `http_headers` 恒字面量、
/// `env_http_headers` 恒引用），照着文本猜正是 `McpValue` 要终结的东西。
pub fn mcp_map(pairs: &[(&str, &str)]) -> BTreeMap<String, McpValue> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), McpValue::from_text_or_literal(v)))
        .collect()
}

pub struct TempHome {
    // 必须保活：drop 即删除整棵目录树
    _dir: TempDir,
    pub home: Home,
    root: PathBuf,
}

// clippy 的 `new_without_default` 要求它（B3a 之前就在，Task 10 跑 `--all-targets`
// 时才暴露：`testutil` 是非默认 feature，只有自指 dev-dependency 会打开它）。
impl Default for TempHome {
    fn default() -> Self {
        Self::new()
    }
}

impl TempHome {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create tempdir");
        let home = Home::new(dir.path());
        // 用 canonicalize 后的 root 构造路径，才能与 adapter 输出（同样 canonicalize 过）直接比较
        let root = home.root().to_path_buf();
        Self {
            _dir: dir,
            home,
            root,
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// 写文件，自动创建父目录。
    pub fn write(&self, rel: &str, body: &str) -> &Self {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::fs::write(&p, body).expect("write file");
        self
    }

    pub fn mkdir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.path(rel)).expect("mkdir");
        self
    }

    /// 创建符号链接（Unix only；本项目只跑 macOS/Linux）。
    /// `target` 原样写入，相对路径由调用方负责正确性。
    pub fn symlink(&self, link: &str, target: &str) -> &Self {
        let l = self.path(link);
        if let Some(parent) = l.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parents");
        }
        std::os::unix::fs::symlink(Path::new(target), &l).expect("symlink");
        self
    }
}

/// 一个本地 git 仓库夹具（§9.10：git 源不联网）。
///
/// 每一条 git 调用都带**显式配置**，因为这些测试跑在开发机上：用户自己的
/// `~/.gitconfig` 里可能有 `commit.gpgsign = true`、`init.defaultBranch = master`
/// 之类的设置，那会让「同样的 fixture 在不同机器上产出不同的 sha」。测试要的是
/// 可复现，不是「在我机器上能过」。
pub struct GitFixture {
    pub dir: PathBuf,
    /// `file://` 形式的 clone 地址——绝对路径直接当 URL 传给 git 会被当成
    /// 「本地路径优化」，行为与远端 clone 有差异，file:// 才是忠实的模拟。
    pub url: String,
}

impl GitFixture {
    fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "henv-fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "henv-fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00+0000")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00+0000")
            .output()
            .expect("git 必须可用：本项目要求开发机装有 git");
        assert!(
            out.status.success(),
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn write(&self, rel: &str, body: &str) {
        let p = self.dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("夹具路径要有父目录")).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// 提交并返回 sha。返回 sha 是为了让测试能断言「账本记的是这个提交」。
    pub fn commit(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    pub fn tag(&self, name: &str) {
        self.git(&["tag", name]);
    }

    /// 建一个分支并切过去。用于「非默认分支」的场景：clone 只为默认分支建
    /// 本地分支，其余分支只活在 `origin/*` 里——那正是解析最容易出错的地方。
    ///
    /// 用完记得 `checkout` 回默认分支：clone 跟随的是**源仓库的 HEAD**，
    /// 留着非默认分支的 HEAD 的话，clone 出来的本地分支就是它，
    /// 「非默认分支」的场景于是被夹具自己抹平了。
    pub fn branch(&self, name: &str) {
        self.git(&["checkout", "-q", "-b", name]);
    }

    /// 切到已有的分支或提交。
    pub fn checkout(&self, name: &str) {
        self.git(&["checkout", "-q", name]);
    }
}

impl TempHome {
    /// 在临时 home 里造一个本地仓库。放在 home 下是因为夹具的一切都该随 TempDir 消失。
    pub fn git_repo(&self, rel: &str) -> GitFixture {
        let dir = self.path(rel);
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("file://{}", dir.display());
        let f = GitFixture { dir, url };
        f.git(&["init", "-q"]);
        f
    }
}

/// 测试替身：`read_state` 返回预置能力，`supports` 按 kind 查表。
///
/// Task 13/14 的 diff 与 golden 测试靠它喂进可控能力，不依赖任何真实配置树——
/// 也就不会去读 `~/.claude` 这类装着凭据的目录。
pub struct FakeAdapter {
    pub adapter_name: String,
    pub detected: bool,
    pub caps: Vec<Capability>,
    /// kind -> 拒绝理由。未列出的 kind 判 Native。
    pub unsupported: BTreeMap<Kind, String>,
    /// `None` = 这家没有用户级 skills 目录（kimi 的真实形态）。
    pub skills_dir: Option<PathBuf>,
    /// `None` = 这家没有用户级 MCP 配置。
    pub mcp_target: Option<McpTarget>,
}

impl FakeAdapter {
    pub fn native(name: &str) -> Self {
        Self {
            adapter_name: name.into(),
            detected: true,
            caps: Vec::new(),
            unsupported: BTreeMap::new(),
            skills_dir: None,
            mcp_target: None,
        }
    }

    pub fn with_skills_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.skills_dir = Some(dir.into());
        self
    }

    pub fn with_mcp_target(mut self, mcp: McpTarget) -> Self {
        self.mcp_target = Some(mcp);
        self
    }

    pub fn with_caps(mut self, caps: Vec<Capability>) -> Self {
        self.caps = caps;
        self
    }

    pub fn with_detected(mut self, detected: bool) -> Self {
        self.detected = detected;
        self
    }

    pub fn refusing(mut self, kind: Kind, reason: &str) -> Self {
        self.unsupported.insert(kind, reason.into());
        self
    }

    /// 造一条「已安装」能力。
    pub fn cap(kind: Kind, name: &str, spec: Spec) -> Capability {
        Capability {
            id: CapId::new(kind, name),
            origin: Origin::Adopted {
                harness: String::new(),
            },
            spec,
            targets: Targets::All,
        }
    }

    /// 造一条由 plugin bundle 携带的 skill。
    pub fn bundled_skill(name: &str, plugin: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Skill, name),
            origin: Origin::Plugin {
                name: plugin.into(),
            },
            spec: Spec::Skill {
                source: crate::capability::SkillSource::PluginBundle {
                    plugin: plugin.into(),
                },
                path: None,
            },
            targets: Targets::All,
        }
    }

    /// 造一条 MCP 能力。
    pub fn mcp(name: &str, command: &str) -> Capability {
        Capability {
            id: CapId::new(Kind::Mcp, name),
            origin: Origin::Manifest,
            spec: Spec::Mcp {
                transport: McpTransport::Stdio {
                    command: command.into(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                },
            },
            targets: Targets::All,
        }
    }

    /// 造一条 plugin 能力。
    pub fn plugin(entry: &str, version: Option<&str>, sha: Option<&str>) -> Capability {
        Capability {
            id: CapId::new(Kind::Plugin, entry),
            origin: Origin::Manifest,
            spec: Spec::Plugin {
                marketplace: None,
                entry: entry.into(),
                version: version.map(String::from),
                sha: sha.map(String::from),
                install_path: None,
                enabled: true,
            },
            targets: Targets::All,
        }
    }
}

impl HarnessAdapter for FakeAdapter {
    fn name(&self) -> &str {
        &self.adapter_name
    }
    fn detect(&self) -> bool {
        self.detected
    }
    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(self.caps.clone())
    }
    fn supports(&self, cap: &Capability) -> Support {
        match self.unsupported.get(&cap.id.kind) {
            Some(reason) => Support::Unsupported {
                reason: reason.clone(),
            },
            None => Support::Native,
        }
    }
    fn skills_target_dir(&self) -> Option<PathBuf> {
        self.skills_dir.clone()
    }
    fn mcp_target(&self) -> Option<McpTarget> {
        self.mcp_target.clone()
    }
}

/// `Confirm` 的测试替身：恒同意 / 恒拒绝。
pub struct AssumeYes;
pub struct RefuseAll;

impl crate::Confirm for AssumeYes {
    fn ask(&mut self, _prompt: &str) -> bool {
        true
    }
}

impl crate::Confirm for RefuseAll {
    fn ask(&mut self, _prompt: &str) -> bool {
        false
    }
}

/// 一条记录 argv 的假 CLI（B3a 的 claude shell out 用）。
///
/// 它证明的是「我们发出的 argv 长什么样」——真 CLI 接不接受、写进哪里，
/// 由 `#[ignore]` 的那条冒烟测试证。两者缺一不可：假 shim 证不了真 CLI 的契约，
/// 真 CLI 证不了我们每一次调用都发了什么（它只说话，不留证据）。
///
/// **HOME 由调用方钉死**：`FakeCli` 只是可执行文件，不设置环境。测试若用它
/// 跑真 CLI 的 wrapper，wrapper 自己 export HOME。
pub struct FakeCli {
    path: PathBuf,
    log: PathBuf,
}

impl FakeCli {
    /// 造一条 shim：记录每次调用的 argv（一次调用一行 `---` 分隔），退出 0。
    pub fn new(t: &TempHome, rel: &str) -> Self {
        Self::with_exit(t, rel, 0, "")
    }

    /// 造一条会失败的 shim，用于证明失败被报出来。
    pub fn failing(t: &TempHome, rel: &str, code: i32, stderr: &str) -> Self {
        Self::with_exit(t, rel, code, stderr)
    }

    fn with_exit(t: &TempHome, rel: &str, code: i32, stderr: &str) -> Self {
        let path = t.path(rel);
        let log = t.path(&format!("{rel}.log"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir shim parent");
        }
        // stderr 先落成文件、再由 shim `cat` 出来，**不把它插进脚本**：
        // 未加引号的 `printf '%s\n' {stderr}` 会按参数逐个换行——
        // "MCP server n already exists" 于是变成五行，错误消息里再也找不到
        // 连续的 `already exists`（实测：这曾让 Task 8 的一条测试红）。
        // 走文件则任何内容都原样吐出，不必跟 shell 引号规则较劲。
        let err_file = t.path(&format!("{rel}.stderr"));
        if !stderr.is_empty() {
            std::fs::write(&err_file, format!("{stderr}\n")).expect("write shim stderr");
        }
        // 逐参数写一行：`"$*"` 会把参数用空格拼起来，丢了边界，
        // 于是「JSON 里带空格」这种真实输入会被验成「参数对不上」。
        let script = format!(
            "#!/bin/sh\nprintf -- '---\\n' >> {log}\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {log}; done\n\
             {}\nexit {code}\n",
            if stderr.is_empty() {
                String::new()
            } else {
                format!("cat {} >&2\n", err_file.display())
            },
            log = log.display()
        );
        std::fs::write(&path, script).expect("write shim");
        let mut perms = std::fs::metadata(&path).expect("stat").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        Self { path, log }
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    /// 每次调用的 argv，按调用顺序。`Vec<Vec<String>>` 里每一项是一次调用。
    pub fn invocations(&self) -> Vec<Vec<String>> {
        let text = match std::fs::read_to_string(&self.log) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<Vec<String>> = Vec::new();
        for line in text.lines() {
            if line == "---" {
                out.push(Vec::new());
            } else if let Some(last) = out.last_mut() {
                last.push(line.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::mcp::McpMechanism;

    #[test]
    fn the_git_fixture_creates_a_hermetic_local_repo() {
        // §9.10：git 源不联网，夹具必须是本地的；也绝不读用户 git 配置。
        let t = TempHome::new();
        let repo = t.git_repo("fixtures/upstream");
        repo.write("skills/foo/SKILL.md", "# foo");
        let first = repo.commit("init");
        assert_eq!(first.len(), 40, "commit 返回完整 sha");

        repo.write("skills/foo/SKILL.md", "# foo v2");
        let second = repo.commit("v2");
        assert_ne!(first, second);
        repo.tag("v1");
        assert!(
            repo.url.starts_with("file://"),
            "必须是本地 URL：{}",
            repo.url
        );

        repo.branch("develop");
        assert_eq!(
            repo.git(&["branch", "--show-current"]),
            "develop",
            "branch() 要真的切过去，否则「非默认分支」的场景是假的"
        );
    }

    #[test]
    fn writes_files_and_symlinks_under_temp_root() {
        let t = TempHome::new();
        t.mkdir(".agents/skills/real-one")
            .write(".agents/skills/real-one/SKILL.md", "# hi")
            .symlink(".claude/skills/real-one", "../../.agents/skills/real-one");

        assert!(t.path(".agents/skills/real-one/SKILL.md").exists());
        let link = t.path(".claude/skills/real-one");
        assert!(link.exists(), "symlink resolves through to the real dir");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../../.agents/skills/real-one")
        );
        assert!(t.home.root().is_absolute());
    }

    /// FakeAdapter 是 Task 13/14 唯一的 fixture 入口：它自己行为错了，那些 diff
    /// 断言就是在替一个不存在的双侧相符/拒绝说话。本 Task 里它**没有生产调用方**，
    /// 所以只有这条测试能证明它兑现了 `HarnessAdapter` 的语义。
    #[test]
    fn fake_adapter_answers_from_its_own_fields() {
        let mcp = FakeAdapter::mcp("context7", "npx");
        let plugin = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some("abc"));
        let caps = vec![mcp.clone(), plugin.clone()];
        let fake = FakeAdapter::native("claude-code")
            .with_caps(caps.clone())
            .refusing(Kind::Plugin, "no plugins");

        assert_eq!(fake.name(), "claude-code");
        assert!(fake.detect(), "native() 默认已装");
        // read_state 原样返回预置能力，且每次调用给的是独立副本
        assert_eq!(fake.read_state().unwrap(), caps);
        // 查表按 kind：被拒的 plugin 带出理由，未列出的 mcp 判 Native
        assert_eq!(
            fake.supports(&plugin),
            Support::Unsupported {
                reason: "no plugins".into()
            }
        );
        assert_eq!(fake.supports(&mcp), Support::Native);
        assert_eq!(
            FakeAdapter::native("codex").supports(&plugin),
            Support::Native,
            "空表必须一律 Native"
        );
        // with_detected 是造出「本机没装这家」的唯一入口（Task 13 的跳过分支要用）
        assert!(
            !FakeAdapter::native("codex").with_detected(false).detect(),
            "with_detected(false) 必须盖过 native() 的 true"
        );
        // MCP 写目标：默认没有，`with_mcp_target` 之后原样返回（Task 9 的
        // `deliver_mcps` 靠这个字段驱动，一个「设了但答 None」的替身会让
        // 那条路径的所有断言都变成空转）。
        assert_eq!(
            FakeAdapter::native("codex").mcp_target(),
            None,
            "默认没有写目标"
        );
        let t = TempHome::new();
        assert_eq!(
            FakeAdapter::native("codex")
                .with_mcp_target(McpTarget {
                    path: t.path(".codex/config.toml"),
                    mechanism: McpMechanism::CodexToml,
                })
                .mcp_target()
                .unwrap()
                .path,
            t.path(".codex/config.toml")
        );
    }

    /// 四个 fixture builder 的产物形状也是契约：Task 13 的 diff 按 kind / origin /
    /// source 分派，一个标错 kind 的 builder 会把「plugin 携带的 skill」当成散装
    /// skill 来比，而 golden 依然自洽——只是钉住了一个错误的结论。
    #[test]
    fn fake_adapter_fixture_builders_tag_kind_origin_and_source() {
        let bare = FakeAdapter::cap(
            Kind::Hook,
            "pre",
            Spec::Hook {
                event: "PreToolUse".into(),
                command: "echo".into(),
            },
        );
        assert_eq!(bare.id, CapId::new(Kind::Hook, "pre"));
        assert_eq!(
            bare.origin,
            Origin::Adopted {
                harness: String::new()
            }
        );
        assert!(bare.targets.includes("kimi-code"));

        let bundled = FakeAdapter::bundled_skill("using-superpowers", "superpowers");
        assert_eq!(bundled.id, CapId::new(Kind::Skill, "using-superpowers"));
        assert_eq!(
            bundled.origin,
            Origin::Plugin {
                name: "superpowers".into()
            }
        );
        assert_eq!(
            bundled.spec,
            Spec::Skill {
                source: crate::capability::SkillSource::PluginBundle {
                    plugin: "superpowers".into()
                },
                path: None,
            }
        );

        let mcp = FakeAdapter::mcp("context7", "npx");
        assert_eq!(mcp.id, CapId::new(Kind::Mcp, "context7"));
        assert_eq!(mcp.origin, Origin::Manifest);

        let plugin = FakeAdapter::plugin("superpowers", Some("6.4.1"), Some("abc"));
        assert_eq!(plugin.id, CapId::new(Kind::Plugin, "superpowers"));
        assert_eq!(
            plugin.spec,
            Spec::Plugin {
                marketplace: None,
                entry: "superpowers".into(),
                version: Some("6.4.1".into()),
                sha: Some("abc".into()),
                install_path: None,
                enabled: true,
            },
            "version/sha 必须原样落到各自字段，串了位就等于给 diff 喂假 locator"
        );
    }

    /// fixture 自己的契约测试：`FakeCli` 记错 argv 边界或调用边界，
    /// Task 8 的每一条 argv 断言都会跟着错，而它们看起来照样「全绿」。
    #[test]
    fn the_fake_cli_records_argv_split_by_invocation() {
        let t = TempHome::new();
        let cli = FakeCli::new(&t, "bin/claude");
        assert!(cli.path().is_file());

        // 直接起它：这是 fixture 自己的契约测试，不用生产代码
        std::process::Command::new(cli.path())
            .args(["mcp", "add-json", "-s", "user", "n", "{\"a\": \"b c\"}"])
            .status()
            .unwrap();
        std::process::Command::new(cli.path())
            .args(["mcp", "remove", "-s", "user", "n"])
            .status()
            .unwrap();

        assert_eq!(
            cli.invocations(),
            vec![
                vec!["mcp", "add-json", "-s", "user", "n", "{\"a\": \"b c\"}"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["mcp", "remove", "-s", "user", "n"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
            ],
            "参数边界与调用边界都要留住"
        );
    }

    #[test]
    fn a_failing_fake_cli_reports_its_code() {
        let t = TempHome::new();
        let cli = FakeCli::failing(&t, "bin/claude", 1, "boom");
        let out = std::process::Command::new(cli.path()).output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "boom");
    }
}
