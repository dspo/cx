use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use dirs::home_dir;
use rand::RngCore;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write as IoWrite};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::fs::{symlink_dir, symlink_file};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PROVIDER_CONFIG_FILE_NAME: &str = "cx.providers.config.yaml";
const LEGACY_PROVIDER_CONFIG_FILE_NAME: &str = "config.yaml";
const LAUNCH_HOME_DIR_NAME: &str = "cx-launch-homes";
const LAUNCH_HOME_TTL_SECS: u64 = 60 * 60 * 24;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_config.rs"));
}

// ══════════════════════════════════════════════════
// 配置结构体（从 YAML 反序列化）
// ══════════════════════════════════════════════════

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct CxConfig {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
    #[serde(default)]
    agents: Vec<AgentConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ProviderConfig {
    name: String,
    #[serde(default)]
    apikey_source: Option<String>,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default)]
    models: BTreeMap<String, ProviderModelConfig>,
    #[serde(default)]
    endpoints: BTreeMap<String, ProviderEndpointSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum ProviderEndpointSpec {
    Url(String),
    Detailed(ProviderEndpointDetail),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ProviderEndpointDetail {
    url: String,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default)]
    copilot_auth: Option<String>,
}

#[derive(Debug, Clone)]
struct EndpointConfig {
    wire_api: String,
    url: String,
    agents: Vec<String>,
    copilot_auth: Option<String>,
    models: Vec<ModelConfig>,
}

#[derive(Debug, Clone)]
struct ModelConfig {
    id: String,
    arena: Option<String>,
    swe_p: Option<String>,
    tb2: Option<String>,
    desc: Option<String>,
    agents: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct ProviderModelConfig {
    #[serde(default)]
    arena: Option<String>,
    #[serde(default)]
    swe_p: Option<String>,
    #[serde(default)]
    tb2: Option<String>,
    #[serde(default)]
    desc: Option<String>,
    #[serde(default)]
    wire_apis: Vec<String>,
    #[serde(default)]
    agents: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AgentConfig {
    id: String,
    #[serde(alias = "bin")]
    binary: String,
    #[serde(default)]
    wire_apis: Vec<String>,
}

impl ProviderConfig {
    fn normalized_endpoints(&self) -> Vec<EndpointConfig> {
        let mut endpoints = self
            .endpoints
            .iter()
            .map(|(wire_api, spec)| {
                let (url, agents, copilot_auth) = match spec {
                    ProviderEndpointSpec::Url(url) => (url.clone(), Vec::new(), None),
                    ProviderEndpointSpec::Detailed(detail) => (
                        detail.url.clone(),
                        detail.agents.clone(),
                        detail.copilot_auth.clone(),
                    ),
                };
                let models = self
                    .models
                    .iter()
                    .filter(|(_, model)| {
                        model.wire_apis.is_empty()
                            || model.wire_apis.iter().any(|candidate| {
                                WireApi::from_str(candidate) == WireApi::from_str(wire_api)
                            })
                    })
                    .map(|(id, model)| ModelConfig {
                        id: id.clone(),
                        arena: model.arena.clone(),
                        swe_p: model.swe_p.clone(),
                        tb2: model.tb2.clone(),
                        desc: model.desc.clone(),
                        agents: model.agents.clone(),
                    })
                    .collect();

                EndpointConfig {
                    wire_api: wire_api.clone(),
                    url,
                    agents,
                    copilot_auth,
                    models,
                }
            })
            .collect::<Vec<_>>();
        endpoints.sort_by_key(|endpoint| WireApi::from_str(&endpoint.wire_api).priority());
        endpoints
    }

    fn has_endpoints(&self) -> bool {
        !self.normalized_endpoints().is_empty()
    }
}

// ══════════════════════════════════════════════════
// 运行时数据结构（从 config 构建，TUI 使用）
// ══════════════════════════════════════════════════

#[derive(Debug, Clone)]
struct ResolvedModel {
    id: String,
    arena: String,
    swe_p: String,
    tb2: String,
    desc: String,
    wire_api: WireApi,
    provider_name: String,
    endpoint_url: String,
    visible_agents: Vec<String>,
    copilot_auth: CopilotAuth,
}

impl ResolvedModel {
    fn from_config(
        config: &CxConfig,
        provider: &ProviderConfig,
        endpoint: &EndpointConfig,
        model: &ModelConfig,
    ) -> Self {
        Self {
            id: model.id.clone(),
            arena: model.arena.clone().unwrap_or_else(|| "—".to_string()),
            swe_p: model.swe_p.clone().unwrap_or_else(|| "—".to_string()),
            tb2: model.tb2.clone().unwrap_or_else(|| "—".to_string()),
            desc: model.desc.clone().unwrap_or_else(|| "".to_string()),
            wire_api: WireApi::from_str(&endpoint.wire_api),
            provider_name: provider.name.clone(),
            endpoint_url: endpoint.url.clone(),
            visible_agents: effective_agents_for_model(config, provider, endpoint, model),
            copilot_auth: CopilotAuth::from_endpoint(endpoint),
        }
    }

    fn formatted_row(&self) -> String {
        format!(
            "{:<24} {:>4} {:>8} {:>6}  {:<11} {}",
            self.id,
            self.arena,
            self.swe_p,
            self.tb2,
            self.wire_api.display(),
            self.desc
        )
    }

    fn supports_agent(&self, agent_id: &str) -> bool {
        let agent_id = canonical_agent_id(agent_id);
        self.visible_agents
            .iter()
            .any(|a| canonical_agent_id(a) == agent_id)
    }

    fn probe_cache_key(&self) -> String {
        probe_cache_key(
            &self.provider_name,
            &self.endpoint_url,
            self.wire_api,
            &self.id,
        )
    }
}

fn canonical_agent_id(agent_id: &str) -> &str {
    match agent_id {
        // Backward-compat for legacy config entries only; the CLI no longer proxies `codex app`.
        "codex-app" => "codex",
        _ => agent_id,
    }
}

fn normalize_agent_ids(agent_ids: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for agent_id in agent_ids {
        let canonical = canonical_agent_id(agent_id).to_string();
        if !normalized.iter().any(|existing| existing == &canonical) {
            normalized.push(canonical);
        }
    }
    normalized
}

fn default_wire_apis_for_agent(agent_id: &str) -> Vec<WireApi> {
    match canonical_agent_id(agent_id) {
        "copilot" => vec![WireApi::Anthropic, WireApi::Responses, WireApi::Completions],
        "claude" => vec![WireApi::Anthropic],
        "codex" => vec![WireApi::Responses],
        _ => Vec::new(),
    }
}

fn resolve_agent_wire_apis(agent_id: &str, configured: &[String]) -> Vec<WireApi> {
    let source = if configured.is_empty() {
        default_wire_apis_for_agent(agent_id)
            .into_iter()
            .map(|wire_api| wire_api.display().to_string())
            .collect::<Vec<_>>()
    } else {
        configured.to_vec()
    };

    let mut resolved = Vec::new();
    for item in source {
        let wire_api = WireApi::from_str(&item);
        if wire_api == WireApi::Unavailable || resolved.contains(&wire_api) {
            continue;
        }
        resolved.push(wire_api);
    }
    resolved
}

fn all_compatible_agents(config: &CxConfig, endpoint: &EndpointConfig) -> Vec<String> {
    let wire_api = WireApi::from_str(&endpoint.wire_api);
    resolved_agents(config)
        .into_iter()
        .filter(|agent| agent.supports_wire_api(wire_api))
        .map(|agent| agent.id)
        .collect()
}

fn effective_agents_for_model(
    config: &CxConfig,
    provider: &ProviderConfig,
    endpoint: &EndpointConfig,
    model: &ModelConfig,
) -> Vec<String> {
    let mut resolved = all_compatible_agents(config, endpoint);

    for filter in [&provider.agents, &endpoint.agents, &model.agents] {
        if filter.is_empty() {
            continue;
        }

        let allowed = normalize_agent_ids(filter);
        resolved.retain(|agent_id| allowed.iter().any(|allowed_id| allowed_id == agent_id));
    }

    resolved
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireApi {
    Responses,
    Completions,
    Anthropic,
    Unavailable,
}

impl WireApi {
    fn from_str(s: &str) -> Self {
        match s {
            "responses" => Self::Responses,
            "completions" => Self::Completions,
            "anthropic" => Self::Anthropic,
            _ => Self::Unavailable,
        }
    }

    fn from_cache(value: &str) -> Option<Self> {
        match value {
            "responses" => Some(Self::Responses),
            "completions" => Some(Self::Completions),
            "anthropic" => Some(Self::Anthropic),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }

    fn display(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::Completions => "completions",
            Self::Anthropic => "anthropic",
            Self::Unavailable => "unavailable",
        }
    }

    fn cache_value(self) -> &'static str {
        self.display()
    }

    fn priority(self) -> u8 {
        match self {
            Self::Anthropic => 0,
            Self::Responses => 1,
            Self::Completions => 2,
            Self::Unavailable => 3,
        }
    }

    fn launch_value(self) -> Result<&'static str> {
        match self {
            Self::Responses => Ok("responses"),
            Self::Completions => Ok("completions"),
            Self::Anthropic => Ok("anthropic"),
            Self::Unavailable => {
                bail!("该模型当前被标记为 unavailable，请先运行 `cx probe` 更新探测结果")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopilotAuth {
    ApiKey,
    BearerToken,
}

fn probe_cache_key(
    provider_name: &str,
    endpoint_url: &str,
    wire_api: WireApi,
    model_id: &str,
) -> String {
    format!(
        "{provider_name}\t{}\t{endpoint_url}\t{model_id}",
        wire_api.display()
    )
}

impl CopilotAuth {
    fn from_endpoint(endpoint: &EndpointConfig) -> Self {
        match endpoint.copilot_auth.as_deref() {
            Some("bearer_token") => Self::BearerToken,
            _ => Self::ApiKey,
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedProvider {
    name: String,
    has_endpoints: bool,
    apikey_source: Option<String>,
}

impl ResolvedProvider {
    fn from_config(config: &ProviderConfig) -> Self {
        Self {
            name: config.name.clone(),
            has_endpoints: config.has_endpoints(),
            apikey_source: config.apikey_source.clone(),
        }
    }

    fn requires_model(&self) -> bool {
        self.has_endpoints
    }
}

#[derive(Debug, Clone)]
struct Selection {
    agent_id: String,
    agent_binary: String,
    provider: ResolvedProvider,
    model: Option<ResolvedModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedAgent {
    id: String,
    binary: String,
    supported_wire_apis: Vec<WireApi>,
}

impl ResolvedAgent {
    fn supports_wire_api(&self, wire_api: WireApi) -> bool {
        self.supported_wire_apis.contains(&wire_api)
    }
}

fn resolved_agents(config: &CxConfig) -> Vec<ResolvedAgent> {
    let mut agents = Vec::new();
    for agent in &config.agents {
        let id = canonical_agent_id(&agent.id).to_string();
        if agents
            .iter()
            .any(|existing: &ResolvedAgent| existing.id == id)
        {
            continue;
        }
        let binary = if id == "codex" {
            "codex".to_string()
        } else {
            agent.binary.clone()
        };
        let supported_wire_apis = resolve_agent_wire_apis(&id, &agent.wire_apis);
        agents.push(ResolvedAgent {
            id,
            binary,
            supported_wire_apis,
        });
    }
    agents
}

fn find_agent(config: &CxConfig, agent_id: &str) -> Option<ResolvedAgent> {
    let agent_id = canonical_agent_id(agent_id);
    resolved_agents(config)
        .into_iter()
        .find(|agent| agent.id == agent_id)
}

fn unsupported_codex_app_message() -> &'static str {
    "`cx` 不再代理 `codex app` 桌面版。请直接运行原生 `codex app ...`；终端版仍可继续使用 `cx codex ...`。"
}

// ══════════════════════════════════════════════════
// 配置加载
// ══════════════════════════════════════════════════

fn provider_config_path() -> Result<PathBuf> {
    Ok(cx_state_dir()?.join(PROVIDER_CONFIG_FILE_NAME))
}

fn legacy_provider_config_path() -> Result<PathBuf> {
    Ok(cx_state_dir()?.join(LEGACY_PROVIDER_CONFIG_FILE_NAME))
}

fn migrate_legacy_provider_config(current_path: &Path, legacy_path: &Path) -> Result<bool> {
    if current_path.exists() || !legacy_path.exists() {
        return Ok(false);
    }

    if let Some(parent) = current_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
    }

    fs::rename(legacy_path, current_path).with_context(|| {
        format!(
            "迁移旧配置失败: {} -> {}",
            legacy_path.display(),
            current_path.display()
        )
    })?;
    eprintln!("已将旧 Provider 配置迁移到 {}", current_path.display());
    Ok(true)
}

fn active_provider_config_path() -> Result<PathBuf> {
    let current_path = provider_config_path()?;
    let legacy_path = legacy_provider_config_path()?;
    migrate_legacy_provider_config(&current_path, &legacy_path)?;
    Ok(current_path)
}

fn read_config_file(path: &Path) -> Result<CxConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
    serde_yaml::from_str(&content).with_context(|| format!("解析配置文件失败: {}", path.display()))
}

fn write_string_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(PROVIDER_CONFIG_FILE_NAME);
    let tmp_path = path.with_file_name(format!(".{file_name}.{}.tmp", random_urlsafe(6)));
    fs::write(&tmp_path, content)
        .with_context(|| format!("写入临时配置文件失败: {}", tmp_path.display()))?;

    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err).with_context(|| {
            format!(
                "替换配置文件失败: {} -> {}",
                tmp_path.display(),
                path.display()
            )
        });
    }

    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("创建目录失败: {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("设置目录权限失败: {}", path.display()))?;
    Ok(())
}

fn write_private_file(path: &Path, content: &str) -> Result<()> {
    write_string_atomic(path, content)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("设置文件权限失败: {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn symlink_path(src: &Path, dst: &Path) -> Result<()> {
    std::os::unix::fs::symlink(src, dst)
        .with_context(|| format!("创建符号链接失败: {} -> {}", dst.display(), src.display()))
}

#[cfg(windows)]
fn symlink_path(src: &Path, dst: &Path) -> Result<()> {
    let result = if src.is_dir() {
        symlink_dir(src, dst)
    } else {
        symlink_file(src, dst)
    };
    result.with_context(|| format!("创建符号链接失败: {} -> {}", dst.display(), src.display()))
}

fn launch_homes_root() -> PathBuf {
    env::temp_dir().join(LAUNCH_HOME_DIR_NAME)
}

fn sweep_old_launch_homes() -> Result<()> {
    let root = launch_homes_root();
    if !root.exists() {
        return Ok(());
    }

    let now = current_unix_secs()?;
    for entry in fs::read_dir(&root).with_context(|| format!("读取目录失败: {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败: {}", root.display()))?;
        let path = entry.path();
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        if let Some(modified) = modified {
            if now.saturating_sub(modified) <= LAUNCH_HOME_TTL_SECS {
                continue;
            }
        }

        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }

    Ok(())
}

fn create_launch_home(agent_id: &str) -> Result<PathBuf> {
    sweep_old_launch_homes()?;
    let root = launch_homes_root();
    ensure_private_dir(&root)?;
    let dir = root.join(format!(
        "{}-{}-{}",
        agent_id,
        current_unix_secs()?,
        random_urlsafe(6)
    ));
    ensure_private_dir(&dir)?;
    Ok(dir)
}

fn mirror_home_entries(real_home: &Path, fake_home: &Path, excluded: &[&str]) -> Result<()> {
    ensure_private_dir(fake_home)?;
    for entry in fs::read_dir(real_home)
        .with_context(|| format!("读取主目录失败: {}", real_home.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败: {}", real_home.display()))?;
        let name = entry.file_name();
        let name_string = name.to_string_lossy();
        if excluded
            .iter()
            .any(|excluded_name| *excluded_name == name_string)
        {
            continue;
        }
        symlink_path(&entry.path(), &fake_home.join(&name))?;
    }
    Ok(())
}

fn materialize_passthrough_dir(
    real_dir: &Path,
    fake_dir: &Path,
    overridden: &[&str],
) -> Result<()> {
    ensure_private_dir(fake_dir)?;
    if !real_dir.exists() {
        return Ok(());
    }

    for entry in
        fs::read_dir(real_dir).with_context(|| format!("读取目录失败: {}", real_dir.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败: {}", real_dir.display()))?;
        let name = entry.file_name();
        let name_string = name.to_string_lossy();
        if overridden
            .iter()
            .any(|overridden_name| *overridden_name == name_string)
        {
            continue;
        }
        symlink_path(&entry.path(), &fake_dir.join(&name))?;
    }
    Ok(())
}

fn toml_basic_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn merge_codex_config(
    existing: Option<&str>,
    model: &ResolvedModel,
    workspace_root: &Path,
) -> Result<String> {
    let project_section = format!(
        "[projects.{}]",
        toml_basic_string(&workspace_root.to_string_lossy())
    );
    let mut retained = Vec::new();
    let mut skipping_section = false;

    if let Some(existing) = existing {
        for line in existing.lines() {
            let trimmed = line.trim();
            if skipping_section {
                if trimmed.starts_with('[') && trimmed.ends_with(']') {
                    skipping_section = false;
                } else {
                    continue;
                }
            }

            if trimmed == "[model_providers.dashscope]" || trimmed == project_section {
                skipping_section = true;
                continue;
            }

            if !trimmed.starts_with('[')
                && (trimmed.starts_with("model =") || trimmed.starts_with("model_provider ="))
            {
                continue;
            }

            retained.push(line.to_string());
        }
    }

    let wire_api = model.wire_api.launch_value()?;
    let mut rendered = format!(
        "model = {}\nmodel_provider = \"dashscope\"\n\n[model_providers.dashscope]\nname = \"DashScope\"\nbase_url = {}\nenv_key = \"DASHSCOPE_API_KEY\"\nwire_api = {}\n\n{}{}\ntrust_level = \"trusted\"\n",
        toml_basic_string(&model.id),
        toml_basic_string(&model.endpoint_url),
        toml_basic_string(wire_api),
        project_section,
        "\n"
    );

    let retained = retained.join("\n").trim().to_string();
    if !retained.is_empty() {
        rendered.push('\n');
        rendered.push_str(&retained);
        rendered.push('\n');
    }

    Ok(rendered)
}

fn merge_claude_settings(
    existing: Option<&str>,
    env_overrides: &BTreeMap<String, String>,
    model_override: Option<&str>,
) -> Result<String> {
    let mut root = match existing {
        Some(existing) if !existing.trim().is_empty() => {
            serde_json::from_str::<Value>(existing).context("解析 Claude settings.json 失败")?
        }
        _ => Value::Object(Map::new()),
    };

    let object = root
        .as_object_mut()
        .context("Claude settings.json 顶层必须是对象")?;

    let env_value = object
        .entry("env".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let env_object = env_value
        .as_object_mut()
        .context("Claude settings.json 中的 env 必须是对象")?;

    for (key, value) in env_overrides {
        env_object.insert(key.clone(), Value::String(value.clone()));
    }

    if let Some(model) = model_override {
        object.insert("model".to_string(), Value::String(model.to_string()));
    }

    serde_json::to_string_pretty(&root).context("序列化 Claude settings.json 失败")
}

fn prepare_claude_launch_home(
    env_overrides: &BTreeMap<String, String>,
    model_override: Option<&str>,
    env: &mut BTreeMap<String, String>,
) -> Result<()> {
    let real_home = home_dir().context("无法解析用户主目录")?;
    let fake_home = create_launch_home("claude")?;
    mirror_home_entries(&real_home, &fake_home, &[".claude"])?;

    let real_claude_dir = real_home.join(".claude");
    let fake_claude_dir = fake_home.join(".claude");
    materialize_passthrough_dir(&real_claude_dir, &fake_claude_dir, &["settings.json"])?;

    let existing_settings = fs::read_to_string(real_claude_dir.join("settings.json")).ok();
    let merged_settings =
        merge_claude_settings(existing_settings.as_deref(), env_overrides, model_override)?;
    write_private_file(&fake_claude_dir.join("settings.json"), &merged_settings)?;

    env.insert("HOME".into(), fake_home.display().to_string());
    env.insert(
        "XDG_CONFIG_HOME".into(),
        fake_home.join(".config").display().to_string(),
    );
    Ok(())
}

fn prepare_codex_launch_home(
    model: &ResolvedModel,
    env: &mut BTreeMap<String, String>,
) -> Result<()> {
    let real_home = home_dir().context("无法解析用户主目录")?;
    let fake_home = create_launch_home("codex")?;
    mirror_home_entries(&real_home, &fake_home, &[".codex"])?;

    let real_codex_dir = real_home.join(".codex");
    let fake_codex_dir = fake_home.join(".codex");
    materialize_passthrough_dir(&real_codex_dir, &fake_codex_dir, &["config.toml"])?;

    let existing_config = fs::read_to_string(real_codex_dir.join("config.toml")).ok();
    let merged_config =
        merge_codex_config(existing_config.as_deref(), model, &env::current_dir()?)?;
    write_private_file(&fake_codex_dir.join("config.toml"), &merged_config)?;

    env.insert("HOME".into(), fake_home.display().to_string());
    env.insert(
        "XDG_CONFIG_HOME".into(),
        fake_home.join(".config").display().to_string(),
    );
    env.insert("CODEX_HOME".into(), fake_codex_dir.display().to_string());
    Ok(())
}

fn load_config() -> Result<CxConfig> {
    let path = active_provider_config_path()?;
    if !path.exists() {
        bail!(
            "Provider 配置不存在: {}\n请先运行 `cx patch --url <url>` 导入，或手动创建该 YAML 文件。",
            path.display()
        );
    }
    read_config_file(&path)
}

fn resolve_apikey(source: &str) -> Result<String> {
    if let Some(rest) = source.strip_prefix("keychain:") {
        keychain_secret(rest)
    } else if let Some(rest) = source.strip_prefix("env:") {
        env::var(rest).with_context(|| format!("环境变量 `{rest}` 未设置"))
    } else if let Some(rest) = source.strip_prefix("literal:") {
        Ok(rest.to_string())
    } else if source.starts_with("$(") {
        // Shell command: $(command)
        let cmd = source.trim_start_matches("$(").trim_end_matches(')');
        let output = Command::new("sh")
            .args(["-c", cmd])
            .output()
            .with_context(|| format!("执行 shell 命令失败: {cmd}"))?;
        if !output.status.success() {
            bail!("shell 命令 `{cmd}` 执行失败");
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    } else {
        bail!("不支持的 apikey_source 格式: `{source}`")
    }
}

// ══════════════════════════════════════════════════
// GitLab Auth
// ══════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitLabAuthState {
    gitlab_base_url: String,
    access_token: String,
    refresh_token: Option<String>,
    token_type: String,
    obtained_at: u64,
    expires_at: Option<u64>,
    user: GitLabUserInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GitLabUserInfo {
    sub: Option<String>,
    preferred_username: Option<String>,
    name: Option<String>,
    email: Option<String>,
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
}

fn cx_state_dir() -> Result<PathBuf> {
    let home = home_dir().context("无法解析用户主目录")?;
    Ok(home.join(".config/cx"))
}

fn auth_state_path() -> Result<PathBuf> {
    Ok(cx_state_dir()?.join("auth.json"))
}

fn gitlab_base_url() -> &'static str {
    embedded::GITLAB_BASE_URL
}

fn gitlab_client_id() -> &'static str {
    embedded::GITLAB_CLIENT_ID
}

fn gitlab_callback_url() -> &'static str {
    embedded::GITLAB_CALLBACK_URL
}

fn gitlab_scopes() -> &'static str {
    embedded::GITLAB_SCOPES
}

fn gitlab_endpoint(path: &str) -> String {
    format!(
        "{}/{}",
        gitlab_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn auth_http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("初始化 GitLab HTTP 客户端失败")
}

fn current_unix_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 Unix Epoch")?
        .as_secs())
}

fn random_urlsafe(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

fn pkce_code_challenge(code_verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()))
}

fn oauth_listener() -> Result<(TcpListener, String)> {
    let callback = Url::parse(gitlab_callback_url()).context("GitLab 回调地址不是合法 URL")?;
    let host = callback.host_str().context("GitLab 回调地址缺少 host")?;
    let port = callback
        .port_or_known_default()
        .context("GitLab 回调地址缺少 port")?;
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr)
        .with_context(|| format!("无法监听 GitLab OAuth 回调地址 {bind_addr}"))?;
    Ok((listener, callback.path().to_string()))
}

fn build_authorize_url(state: &str, code_challenge: &str) -> Result<String> {
    let mut url =
        Url::parse(&gitlab_endpoint("/oauth/authorize")).context("构造 GitLab 授权地址失败")?;
    url.query_pairs_mut()
        .append_pair("client_id", gitlab_client_id())
        .append_pair("redirect_uri", gitlab_callback_url())
        .append_pair("response_type", "code")
        .append_pair("state", state)
        .append_pair("scope", gitlab_scopes())
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

fn wait_for_oauth_callback(
    listener: &TcpListener,
    expected_path: &str,
) -> Result<BTreeMap<String, String>> {
    let (mut stream, _) = listener.accept().context("等待 GitLab OAuth 回调失败")?;
    let mut buffer = [0u8; 8192];
    let read_len = stream
        .read(&mut buffer)
        .context("读取 GitLab OAuth 回调失败")?;
    let request = String::from_utf8_lossy(&buffer[..read_len]);
    let request_line = request
        .lines()
        .next()
        .context("GitLab OAuth 回调请求为空")?;
    let target = request_line
        .split_whitespace()
        .nth(1)
        .context("GitLab OAuth 回调请求格式不正确")?;
    let callback_url =
        Url::parse(&format!("http://127.0.0.1{target}")).context("解析 GitLab 回调参数失败")?;
    if callback_url.path() != expected_path {
        write_callback_response(
            &mut stream,
            "无效回调路径",
            "收到的 OAuth 回调路径与预期不一致，请重新运行 `cx login`。",
        )?;
        bail!(
            "GitLab OAuth 回调路径不匹配：期望 `{expected_path}`，实际 `{}`",
            callback_url.path()
        );
    }

    let params = callback_url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<BTreeMap<_, _>>();
    if let Some(error) = params.get("error") {
        write_callback_response(
            &mut stream,
            "GitLab 登录失败",
            "授权页返回了错误，请回到终端查看详情。",
        )?;
        bail!(
            "GitLab OAuth 授权失败: {}",
            params
                .get("error_description")
                .cloned()
                .unwrap_or_else(|| error.clone())
        );
    }

    write_callback_response(
        &mut stream,
        "cx 登录成功",
        "cx 已完成 GitLab 登录，你现在可以关闭这个窗口返回终端。",
    )?;
    Ok(params)
}

fn write_callback_response(stream: &mut impl IoWrite, title: &str, body: &str) -> Result<()> {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body><h1>{title}</h1><p>{body}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    stream
        .write_all(response.as_bytes())
        .context("写入 GitLab OAuth 回调响应失败")
}

fn exchange_oauth_code(
    client: &reqwest::blocking::Client,
    code: &str,
    code_verifier: &str,
) -> Result<GitLabTokenResponse> {
    let response = client
        .post(gitlab_endpoint("/oauth/token"))
        .form(&[
            ("client_id", gitlab_client_id()),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", gitlab_callback_url()),
            ("code_verifier", code_verifier),
        ])
        .send()
        .context("向 GitLab 交换 access token 失败")?
        .error_for_status()
        .context("GitLab token 接口返回了错误状态")?;
    response
        .json::<GitLabTokenResponse>()
        .context("解析 GitLab token 响应失败")
}

fn refresh_oauth_token(
    client: &reqwest::blocking::Client,
    refresh_token: &str,
) -> Result<GitLabTokenResponse> {
    let response = client
        .post(gitlab_endpoint("/oauth/token"))
        .form(&[
            ("client_id", gitlab_client_id()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
            ("redirect_uri", gitlab_callback_url()),
        ])
        .send()
        .context("向 GitLab 刷新 access token 失败")?
        .error_for_status()
        .context("GitLab refresh token 接口返回了错误状态")?;
    response
        .json::<GitLabTokenResponse>()
        .context("解析 GitLab refresh token 响应失败")
}

fn fetch_gitlab_userinfo(
    client: &reqwest::blocking::Client,
    access_token: &str,
) -> Result<GitLabUserInfo> {
    let response = client
        .get(gitlab_endpoint("/oauth/userinfo"))
        .bearer_auth(access_token)
        .send()
        .context("读取 GitLab 用户信息失败")?
        .error_for_status()
        .context("GitLab userinfo 接口返回了错误状态")?;
    response
        .json::<GitLabUserInfo>()
        .context("解析 GitLab 用户信息失败")
}

fn auth_state_from_token(
    token: GitLabTokenResponse,
    user: GitLabUserInfo,
    obtained_at: u64,
) -> GitLabAuthState {
    GitLabAuthState {
        gitlab_base_url: gitlab_base_url().to_string(),
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        token_type: token.token_type,
        obtained_at,
        expires_at: token.expires_in.map(|seconds| obtained_at + seconds),
        user,
    }
}

fn load_auth_state() -> Result<GitLabAuthState> {
    let path = auth_state_path()?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("读取登录状态失败: {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("解析登录状态失败: {}", path.display()))
}

fn save_auth_state(state: &GitLabAuthState) -> Result<()> {
    let path = auth_state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建登录状态目录失败: {}", parent.display()))?;
        #[cfg(unix)]
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("设置目录权限失败: {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("写入登录状态失败: {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("设置登录状态文件权限失败: {}", path.display()))?;
    Ok(())
}

fn remove_auth_state() -> Result<bool> {
    let path = auth_state_path()?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("删除登录状态失败: {}", path.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn refresh_auth_state_if_needed(state: &mut GitLabAuthState) -> Result<()> {
    if state.gitlab_base_url.trim_end_matches('/') != gitlab_base_url().trim_end_matches('/') {
        bail!(
            "当前登录状态绑定的是 `{}`，与内嵌 GitLab 域名 `{}` 不一致，请重新运行 `cx login`。",
            state.gitlab_base_url,
            gitlab_base_url()
        );
    }

    let now = current_unix_secs()?;
    let expires_at = match state.expires_at {
        Some(expires_at) => expires_at,
        None => return Ok(()),
    };
    if expires_at > now + 60 {
        return Ok(());
    }

    let refresh_token = state
        .refresh_token
        .clone()
        .context("GitLab 登录状态已过期，且缺少 refresh token，请重新运行 `cx login`。")?;
    let client = auth_http_client()?;
    let token = refresh_oauth_token(&client, &refresh_token)?;
    let obtained_at = current_unix_secs()?;
    let user = fetch_gitlab_userinfo(&client, &token.access_token)?;
    *state = auth_state_from_token(token, user, obtained_at);
    save_auth_state(state)?;
    Ok(())
}

fn require_login() -> Result<GitLabAuthState> {
    let mut state = load_auth_state()
        .map_err(|err| anyhow!("请先运行 `cx login` 完成 GitLab 认证。\n原始错误: {err}"))?;
    refresh_auth_state_if_needed(&mut state)?;
    Ok(state)
}

fn display_user(user: &GitLabUserInfo) -> String {
    if let Some(username) = &user.preferred_username {
        username.clone()
    } else if let Some(name) = &user.name {
        name.clone()
    } else if let Some(email) = &user.email {
        email.clone()
    } else {
        "unknown-user".to_string()
    }
}

fn run_login() -> Result<()> {
    let client = auth_http_client()?;
    let state = random_urlsafe(24);
    let code_verifier = random_urlsafe(48);
    let code_challenge = pkce_code_challenge(&code_verifier);
    let authorize_url = build_authorize_url(&state, &code_challenge)?;
    let (listener, callback_path) = oauth_listener()?;

    println!("请在浏览器中完成 GitLab 登录：\n{authorize_url}\n");
    if let Err(err) = webbrowser::open(&authorize_url) {
        println!("无法自动打开浏览器，请手动复制上面的 URL 登录：{err}");
    }

    let params = wait_for_oauth_callback(&listener, &callback_path)?;
    let returned_state = params.get("state").context("GitLab OAuth 回调缺少 state")?;
    if returned_state != &state {
        bail!("GitLab OAuth state 校验失败，请重新运行 `cx login`。");
    }
    let code = params.get("code").context("GitLab OAuth 回调缺少 code")?;
    let token = exchange_oauth_code(&client, code, &code_verifier)?;
    let obtained_at = current_unix_secs()?;
    let user = fetch_gitlab_userinfo(&client, &token.access_token)?;
    let auth_state = auth_state_from_token(token, user.clone(), obtained_at);
    save_auth_state(&auth_state)?;

    println!("GitLab 登录成功：{}", display_user(&user));
    Ok(())
}

fn run_logout() -> Result<()> {
    if remove_auth_state()? {
        println!("已清除本地 GitLab 登录状态。");
    } else {
        println!("当前没有本地 GitLab 登录状态。");
    }
    Ok(())
}

fn run_whoami() -> Result<()> {
    let mut state = load_auth_state().map_err(|err| {
        anyhow!("当前没有可用的 GitLab 登录状态，请先运行 `cx login`。\n原始错误: {err}")
    })?;
    refresh_auth_state_if_needed(&mut state)?;

    println!("GitLab: {}", state.gitlab_base_url);
    println!("用户: {}", display_user(&state.user));
    if let Some(name) = &state.user.name {
        println!("姓名: {name}");
    }
    if let Some(email) = &state.user.email {
        println!("邮箱: {email}");
    }
    if let Some(profile) = &state.user.profile {
        println!("主页: {profile}");
    }
    if let Some(expires_at) = state.expires_at {
        println!("过期时间(Unix): {expires_at}");
    }
    Ok(())
}

fn build_all_models(config: &CxConfig) -> Vec<ResolvedModel> {
    let mut models = Vec::new();
    for provider in &config.providers {
        for endpoint in provider.normalized_endpoints() {
            for model in &endpoint.models {
                models.push(ResolvedModel::from_config(
                    config, provider, &endpoint, model,
                ));
            }
        }
    }
    models
}

fn provider_supports_agent(config: &CxConfig, provider: &ProviderConfig, agent_id: &str) -> bool {
    let agent_id = canonical_agent_id(agent_id);
    if !provider.has_endpoints() {
        return provider.agents.is_empty()
            || provider
                .agents
                .iter()
                .any(|candidate| canonical_agent_id(candidate) == agent_id);
    }

    provider.normalized_endpoints().iter().any(|endpoint| {
        endpoint.models.iter().any(|model| {
            effective_agents_for_model(config, provider, endpoint, model)
                .iter()
                .any(|candidate| canonical_agent_id(candidate) == agent_id)
        })
    })
}

fn apply_probe_cache(models: &mut Vec<ResolvedModel>) {
    if let Ok(cache) = load_probe_cache() {
        for model in models.iter_mut() {
            if let Some(wire_api) = cache
                .models
                .get(&model.probe_cache_key())
                .and_then(|value| WireApi::from_cache(value))
            {
                model.wire_api = wire_api;
            }
        }
    }
}

fn providers_for_agent(config: &CxConfig, agent_id: &str) -> Vec<ResolvedProvider> {
    let agent_id = canonical_agent_id(agent_id);
    let mut providers: Vec<ResolvedProvider> = config
        .providers
        .iter()
        .filter(|provider| provider_supports_agent(config, provider, agent_id))
        .map(ResolvedProvider::from_config)
        .collect();

    // Append the "add provider" sentinel
    providers.push(ResolvedProvider {
        name: "+ 添加 Provider".to_string(),
        has_endpoints: false,
        apikey_source: None,
    });
    providers
}

fn models_for_provider(
    all_models: &[ResolvedModel],
    agent_id: &str,
    provider_name: &str,
) -> Vec<ResolvedModel> {
    let mut filtered: Vec<ResolvedModel> = Vec::new();
    let mut indexes_by_id: BTreeMap<String, usize> = BTreeMap::new();

    for model in all_models
        .iter()
        .filter(|m| m.provider_name == provider_name && m.supports_agent(agent_id))
    {
        if let Some(index) = indexes_by_id.get(&model.id).copied() {
            if model.wire_api.priority() < filtered[index].wire_api.priority() {
                filtered[index] = model.clone();
            }
            continue;
        }

        indexes_by_id.insert(model.id.clone(), filtered.len());
        filtered.push(model.clone());
    }

    filtered
}

// ══════════════════════════════════════════════════
// CLI definition（clap derive）
// ══════════════════════════════════════════════════

#[derive(Parser)]
#[command(
    name = "cx",
    about = "统一 Agent 入口",
    version = VERSION,
    disable_help_subcommand = true,
    disable_version_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CxCommand>,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
enum CxCommand {
    /// 显示帮助信息
    Help,
    /// 完成 GitLab 登录
    Login,
    /// 清除本地 GitLab 登录状态
    Logout,
    /// 显示当前登录用户
    Whoami,
    /// 探测模型的 completions / responses 支持情况
    Probe {
        /// 模型 ID（可选，不指定则探测全部）
        model_id: Option<String>,
    },
    /// 从远程 URL 下载 Provider 配置并合并到本地
    Patch {
        /// 远程配置 YAML 文件的 URL
        #[arg(long)]
        url: Option<String>,
        /// 使用上次记录的 URL 重新获取配置
        #[arg(long)]
        refresh: bool,
    },
}

// ══════════════════════════════════════════════════
// 入口
// ══════════════════════════════════════════════════

pub fn run() -> Result<()> {
    let raw_args: Vec<String> = env::args().collect();

    // patch command does not require a config file or login
    if raw_args.get(1).map(String::as_str) == Some("patch") {
        if let Ok(cli) = Cli::try_parse_from(&raw_args) {
            if let Some(CxCommand::Patch { url, refresh }) = cli.command {
                return run_patch(url, refresh);
            }
        }
    }

    let config = load_config()?;

    match Cli::try_parse_from(&raw_args) {
        Ok(cli) => match cli.command {
            Some(CxCommand::Help) | None => {
                print_help();
                Ok(())
            }
            Some(CxCommand::Login) => run_login(),
            Some(CxCommand::Logout) => run_logout(),
            Some(CxCommand::Whoami) => run_whoami(),
            Some(CxCommand::Probe { model_id }) => {
                let _auth = require_login()?;
                run_probe(model_id, &config)
            }
            Some(CxCommand::Patch { .. }) => {
                unreachable!("patch is handled before config load")
            }
        },
        Err(_) => {
            // Not a known subcommand → treat as Launch with agent hint
            let args: Vec<String> = raw_args[1..].to_vec();
            let _auth = require_login()?;

            if let Some(first) = args.first() {
                if first == "codex-app"
                    || (first == "codex" && args.get(1).map(String::as_str) == Some("app"))
                {
                    bail!(unsupported_codex_app_message());
                }
                if let Some(agent) = find_agent(&config, first) {
                    return run_launcher(Some(agent.id), &config, args[1..].to_vec());
                }
            }

            run_launcher(None, &config, args)
        }
    }
}

// ══════════════════════════════════════════════════
// Launcher
// ══════════════════════════════════════════════════

fn run_launcher(
    agent_hint: Option<String>,
    config: &CxConfig,
    passthrough_args: Vec<String>,
) -> Result<()> {
    let mut all_models = build_all_models(config);
    apply_probe_cache(&mut all_models);

    let selection = run_tui(agent_hint, config, &all_models)?;

    let Some(selection) = selection else {
        return Ok(());
    };

    if selection.provider.name == "+ 添加 Provider" {
        print_add_provider_guide(&selection.agent_id);
        return Ok(());
    }

    let spec = build_launch_spec(&selection, &passthrough_args)?;

    println!();
    println!("{}", spec.summary);
    println!();

    exec_launch(spec)
}

fn print_add_provider_guide(agent_id: &str) {
    println!();
    println!("为 {agent_id} 添加 Provider：");
    println!();
    println!("1. 运行 `cx patch --url <url>` 从远程下载 Provider 配置。");
    println!("2. 或手动编辑 ~/.config/cx/cx.providers.config.yaml 的 providers 列表。");
    println!("3. 仓库中的 `config/providers.default.yaml` 可作为基线示例。");
    println!("4. 配置格式参考: docs/cx-config-schema.yaml");
    println!();
}

fn print_help() {
    Cli::parse_from(["cx", "--help"]);
}

// ══════════════════════════════════════════════════
// Patch — 远程 Provider 配置合并
// ══════════════════════════════════════════════════

fn patch_source_path() -> Result<PathBuf> {
    let home = home_dir().context("无法解析用户主目录")?;
    Ok(home.join(".config/cx/.patch_source"))
}

fn save_patch_source(url: &str) -> Result<()> {
    let path = patch_source_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
    }
    fs::write(&path, url).with_context(|| "保存 patch 来源 URL 失败")?;
    Ok(())
}

fn load_patch_source() -> Result<String> {
    let path = patch_source_path()?;
    fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .with_context(|| {
            format!(
                "未找到 patch 来源 URL，请先运行 `cx patch --url <url>`: {}",
                path.display()
            )
        })
}

fn run_patch(url: Option<String>, refresh: bool) -> Result<()> {
    let url = if refresh {
        load_patch_source()?
    } else if let Some(ref u) = url {
        u.clone()
    } else {
        bail!("请指定 --url <url> 或 --refresh")
    };

    println!("从 {} 下载 Provider 配置...", url);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("初始化 HTTP 客户端失败")?;

    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("下载配置失败: {url}"))?;

    let status = response.status();
    if !status.is_success() {
        bail!("下载配置失败: HTTP {}", status.as_u16());
    }

    let body = response.text().context("读取响应失败")?;
    let incoming: CxConfig = serde_yaml::from_str(&body).with_context(|| "解析远程配置失败")?;

    let config_path = active_provider_config_path()?;
    let existing = if config_path.exists() {
        read_config_file(&config_path)?
    } else {
        CxConfig::default()
    };

    let merged = CxConfig {
        providers: merge_providers(&existing.providers, &incoming.providers),
        agents: merge_agents(&existing.agents, &incoming.agents),
    };

    let yaml = serde_yaml::to_string(&merged).context("序列化配置失败")?;
    write_string_atomic(&config_path, &yaml)?;

    save_patch_source(&url)?;

    println!("配置已更新: {}", config_path.display());
    println!("来源 URL 已记录，后续可通过 `cx patch --refresh` 更新。");

    Ok(())
}

/// Replace providers by `name`; new providers are appended.
/// Preserves existing order; incoming replacements stay in-place, new items are appended at end.
fn merge_providers(
    existing: &[ProviderConfig],
    incoming: &[ProviderConfig],
) -> Vec<ProviderConfig> {
    let mut replaced = vec![false; incoming.len()];
    let mut result = Vec::with_capacity(existing.len() + incoming.len());

    for existing_provider in existing {
        if let Some((index, replacement)) = incoming
            .iter()
            .enumerate()
            .find(|(_, provider)| provider.name == existing_provider.name)
        {
            replaced[index] = true;
            result.push(replacement.clone());
        } else {
            result.push(existing_provider.clone());
        }
    }

    for (index, provider) in incoming.iter().enumerate() {
        if !replaced[index] {
            result.push(provider.clone());
        }
    }

    result
}

/// Replace agents by `id`; new agents are appended.
/// Preserves existing order; incoming replacements stay in-place, new items are appended at end.
fn merge_agents(existing: &[AgentConfig], incoming: &[AgentConfig]) -> Vec<AgentConfig> {
    let mut replaced = vec![false; incoming.len()];
    let mut result = Vec::with_capacity(existing.len() + incoming.len());

    for existing_agent in existing {
        if let Some((index, replacement)) = incoming
            .iter()
            .enumerate()
            .find(|(_, agent)| agent.id == existing_agent.id)
        {
            replaced[index] = true;
            result.push(replacement.clone());
        } else {
            result.push(existing_agent.clone());
        }
    }

    for (index, agent) in incoming.iter().enumerate() {
        if !replaced[index] {
            result.push(agent.clone());
        }
    }

    result
}

// ══════════════════════════════════════════════════
// Secret Prompting — 交互式补齐缺失的 API Key
// ══════════════════════════════════════════════════

fn resolve_apikey_interactive(source: &str) -> Result<String> {
    match resolve_apikey(source) {
        Ok(key) => Ok(key),
        Err(e) => {
            if let Some(service) = source.strip_prefix("keychain:") {
                if !cfg!(target_os = "macos") {
                    bail!("`keychain:` 仅支持 macOS Keychain，请改用 `env:` 或在 macOS 上运行。");
                }
                eprintln!("从 Keychain 读取 `{service}` 失败: {e}");
                eprint!("请输入 {service} 的 API Key: ");

                let mut input = String::new();
                io::stdin().read_line(&mut input).context("读取输入失败")?;
                let key = input.trim().to_string();

                if key.is_empty() {
                    bail!("API Key 不能为空");
                }

                let user = env::var("USER").unwrap_or_default();
                let child = Command::new("security")
                    .args([
                        "add-generic-password",
                        "-a",
                        &user,
                        "-s",
                        service,
                        "-w",
                        &key,
                        "-U",
                    ])
                    .spawn()
                    .with_context(|| "写入 Keychain 失败")?;

                let output = child
                    .wait_with_output()
                    .with_context(|| "等待 Keychain 写入完成失败")?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("写入 Keychain 失败: {}", stderr.trim());
                }

                eprintln!("已将 API Key 保存到 Keychain `{service}`。");
                Ok(key)
            } else if let Some(var) = source.strip_prefix("env:") {
                bail!("环境变量 `{var}` 未设置，请通过 `export {var}=<your-key>` 设置后重试")
            } else {
                Err(e)
            }
        }
    }
}

fn exec_launch(spec: LaunchSpec) -> Result<()> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    command.envs(&spec.env);

    if spec.detach {
        // GUI app: spawn in background, then exit terminal
        #[cfg(unix)]
        {
            command.process_group(0); // separate process group
            let _child = command
                .spawn()
                .with_context(|| format!("启动 `{}` 失败", spec.program.display()))?;
            // Don't wait — just exit
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let _child = command
                .spawn()
                .with_context(|| format!("启动 `{}` 失败", spec.program.display()))?;
            return Ok(());
        }
    }

    // CLI: exec replaces current process
    #[cfg(unix)]
    {
        let error = command.exec();
        Err(anyhow!("启动 `{}` 失败: {error}", spec.program.display()))
    }

    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .with_context(|| format!("启动 `{}` 失败", spec.program.display()))?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[derive(Debug)]
struct LaunchSpec {
    program: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    summary: String,
    detach: bool,
    env_remove: Vec<String>,
}

fn build_launch_spec(selection: &Selection, passthrough_args: &[String]) -> Result<LaunchSpec> {
    let program = resolve_binary(&selection.agent_binary)?;
    let mut args = Vec::new();
    let mut env = BTreeMap::new();

    let agent_id = &selection.agent_id;
    let provider = &selection.provider;
    let mut env_remove = Vec::new();

    // Default provider (no endpoints) — use agent's own default behavior
    if !provider.has_endpoints {
        match agent_id.as_str() {
            "copilot" => {
                args.extend(passthrough_args.iter().cloned());
            }
            "claude" => {
                env_remove.push("ANTHROPIC_API_KEY".into());
                env_remove.push("ANTHROPIC_AUTH_TOKEN".into());
                let mut settings_env = BTreeMap::new();
                if let Some(ref source) = provider.apikey_source {
                    let key = resolve_apikey_interactive(source)?;
                    settings_env.insert("ANTHROPIC_API_KEY".into(), key.clone());
                    settings_env.insert("ANTHROPIC_AUTH_TOKEN".into(), key);
                }
                prepare_claude_launch_home(&settings_env, None, &mut env)?;
                args.extend(passthrough_args.iter().cloned());
            }
            "codex" => {
                if let Some(ref source) = provider.apikey_source {
                    if let Ok(value) = resolve_apikey_interactive(source) {
                        env.insert("AZURE_OPENAI_API_KEY".into(), value);
                    }
                }
                args.extend(passthrough_args.iter().cloned());
            }
            _ => {
                args.extend(passthrough_args.iter().cloned());
            }
        }
    } else {
        // Provider with endpoints — inject env/args based on wire_api and config
        let model = selection.model.as_ref().context(format!(
            "{} 选择了 {}，但没有选中模型",
            agent_id, provider.name
        ))?;

        let apikey = if let Some(ref source) = provider.apikey_source {
            resolve_apikey_interactive(source)?
        } else {
            bail!(
                "Provider `{}` 需要 API Key 但未配置 apikey_source",
                provider.name
            );
        };

        match agent_id.as_str() {
            "copilot" => {
                env.insert(
                    "COPILOT_PROVIDER_BASE_URL".into(),
                    model.endpoint_url.clone(),
                );
                env.insert("COPILOT_MODEL".into(), model.id.clone());
                configure_copilot_auth(&mut env, model.copilot_auth, apikey);
                match model.wire_api {
                    WireApi::Anthropic => {
                        env.insert("COPILOT_PROVIDER_TYPE".into(), "anthropic".into());
                    }
                    WireApi::Responses | WireApi::Completions => {
                        env.insert("COPILOT_PROVIDER_TYPE".into(), "openai".into());
                        env.insert(
                            "COPILOT_PROVIDER_WIRE_API".into(),
                            model.wire_api.launch_value()?.to_string(),
                        );
                    }
                    WireApi::Unavailable => {
                        bail!(
                            "`copilot` 当前无法使用 `{}`，因为它被标记为 unavailable。",
                            model.id
                        );
                    }
                }
                args.extend(passthrough_args.iter().cloned());
            }
            "claude" => {
                env_remove.push("ANTHROPIC_API_KEY".into());
                env_remove.push("ANTHROPIC_AUTH_TOKEN".into());
                let mut settings_env = BTreeMap::new();
                settings_env.insert("ANTHROPIC_BASE_URL".into(), model.endpoint_url.clone());
                settings_env.insert("ANTHROPIC_API_KEY".into(), apikey);
                prepare_claude_launch_home(&settings_env, Some(&model.id), &mut env)?;
                args.extend(passthrough_args.iter().cloned());
            }
            "codex" => {
                if model.wire_api != WireApi::Responses {
                    bail!(
                        "`codex` 当前仅支持 responses endpoint 模型；`{}` 在内嵌配置中标记为 {}。",
                        model.id,
                        model.wire_api.display()
                    );
                }
                env.insert("DASHSCOPE_API_KEY".into(), apikey);
                prepare_codex_launch_home(model, &mut env)?;
                args.extend(passthrough_args.iter().cloned());
            }
            _ => {
                // Generic fallback: just pass through
                args.extend(passthrough_args.iter().cloned());
            }
        }
    }

    let summary = match &selection.model {
        Some(model) => format!(
            "启动 {} | Provider: {} | Model: {}",
            agent_id, provider.name, model.id
        ),
        None => format!("启动 {} | Provider: {}", agent_id, provider.name),
    };

    Ok(LaunchSpec {
        program,
        args,
        env,
        summary,
        detach: false,
        env_remove,
    })
}

fn configure_copilot_auth(
    env: &mut BTreeMap<String, String>,
    auth: CopilotAuth,
    credential: String,
) {
    match auth {
        CopilotAuth::ApiKey => {
            env.insert("COPILOT_PROVIDER_API_KEY".into(), credential);
        }
        CopilotAuth::BearerToken => {
            env.insert("COPILOT_PROVIDER_BEARER_TOKEN".into(), credential);
        }
    }
}

fn resolve_binary(name: &str) -> Result<PathBuf> {
    // CLI binary: use which + fallback paths
    if let Ok(path) = which::which(name) {
        return Ok(path);
    }

    let home = home_dir().context("无法解析用户主目录")?;
    let fallbacks = [
        home.join(".nvm/versions/node/v20.19.4/bin").join(name),
        home.join(".local/bin").join(name),
        PathBuf::from("/opt/homebrew/bin").join(name),
    ];

    fallbacks
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| anyhow!("找不到原生可执行文件 `{name}`"))
}

fn keychain_secret(service: &str) -> Result<String> {
    if !cfg!(target_os = "macos") {
        bail!("`keychain:` 仅支持 macOS Keychain，请改用 `env:` 配置 `{service}`。");
    }

    let user = env::var("USER").unwrap_or_default();
    let output = Command::new("security")
        .args(["find-generic-password", "-a", &user, "-s", service, "-w"])
        .output()
        .with_context(|| format!("调用 macOS Keychain 失败，service={service}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "无法从 Keychain 读取 `{service}`: {}",
            if stderr.is_empty() {
                "未知错误".to_string()
            } else {
                stderr
            }
        );
    }

    let secret = String::from_utf8(output.stdout)
        .with_context(|| format!("Keychain 返回的 `{service}` 不是合法 UTF-8"))?
        .trim()
        .to_string();

    if secret.is_empty() {
        bail!("Keychain 中的 `{service}` 为空");
    }

    Ok(secret)
}

// ══════════════════════════════════════════════════
// Probe
// ══════════════════════════════════════════════════

#[derive(Debug, Deserialize, Serialize, Default)]
struct ProbeCache {
    timestamp: String,
    models: BTreeMap<String, String>,
}

fn probe_cache_path() -> Result<PathBuf> {
    let home = home_dir().context("无法解析用户主目录")?;
    Ok(home.join(".config/cx/probe_cache.json"))
}

fn load_probe_cache() -> Result<ProbeCache> {
    let path = probe_cache_path()?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("读取 probe 缓存失败: {}", path.display()))?;
    let cache = serde_json::from_str::<ProbeCache>(&content)
        .with_context(|| format!("解析 probe 缓存失败: {}", path.display()))?;
    Ok(cache)
}

fn save_probe_cache(cache: &ProbeCache) -> Result<()> {
    let path = probe_cache_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 probe 缓存目录失败: {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(cache)?)
        .with_context(|| format!("写入 probe 缓存失败: {}", path.display()))?;
    println!("\n缓存已保存到 {}", path.display());
    Ok(())
}

fn run_probe(target_model: Option<String>, config: &CxConfig) -> Result<()> {
    // Find the first OpenAI-compatible endpoint that can be probed with completions/responses.
    let (probe_provider, probe_endpoint) = config
        .providers
        .iter()
        .find_map(|provider| {
            if provider.apikey_source.is_none() {
                return None;
            }
            provider
                .normalized_endpoints()
                .into_iter()
                .find(|endpoint| {
                    matches!(
                        WireApi::from_str(&endpoint.wire_api),
                        WireApi::Responses | WireApi::Completions
                    )
                })
                .map(|endpoint| (provider, endpoint))
        })
        .context(
            "没有可探测的 Provider（需要至少一个带 API Key 的 responses/completions endpoint）",
        )?;

    let apikey = resolve_apikey_interactive(probe_provider.apikey_source.as_ref().unwrap())?;
    let probe_base_url = &probe_endpoint.url;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("初始化 HTTP 客户端失败")?;

    let mut deduped_models = BTreeMap::new();
    for model in build_all_models(config).into_iter().filter(|model| {
        model.provider_name == probe_provider.name
            && model.endpoint_url == *probe_base_url
            && model.wire_api != WireApi::Anthropic
    }) {
        deduped_models.entry(model.id.clone()).or_insert(model);
    }
    let mut all_models = deduped_models.into_values().collect::<Vec<_>>();
    if let Some(ref target) = target_model {
        all_models.retain(|model| model.id == *target);
        if all_models.is_empty() {
            let available_ids = build_all_models(config)
                .into_iter()
                .filter(|model| {
                    model.provider_name == probe_provider.name
                        && model.endpoint_url == *probe_base_url
                        && model.wire_api != WireApi::Anthropic
                })
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("未知模型 `{target}`，可用模型：{available_ids}");
        }
    }

    println!("开始探测 {} 个模型...\n", all_models.len());

    let mut cache = ProbeCache {
        timestamp: chrono_like_timestamp(),
        models: BTreeMap::new(),
    };

    for model in &mut all_models {
        println!("探测 {}...", model.id);
        let result = probe_model(&client, &apikey, &model.id, probe_base_url)?;

        let completions_status = if result.completions.ok {
            "✅ completions"
        } else {
            "❌ completions"
        };
        let responses_status = if result.responses.ok {
            "✅ responses"
        } else {
            "❌ responses"
        };
        println!("  {} | {}", completions_status, responses_status);

        if !result.completions.ok {
            if let Some(error) = &result.completions.error {
                println!("  completions: {error}");
            }
        }
        if !result.responses.ok {
            if let Some(error) = &result.responses.error {
                println!("  responses: {error}");
            }
        }

        model.wire_api = if result.responses.ok {
            WireApi::Responses
        } else if result.completions.ok {
            WireApi::Completions
        } else {
            WireApi::Unavailable
        };

        cache.models.insert(
            model.probe_cache_key(),
            model.wire_api.cache_value().to_string(),
        );
        println!();
    }

    save_probe_cache(&cache)?;
    Ok(())
}

fn chrono_like_timestamp() -> String {
    let now = std::time::SystemTime::now();
    match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format!("{}", duration.as_secs()),
        Err(_) => "0".to_string(),
    }
}

#[derive(Debug)]
struct ProbeResult {
    completions: EndpointResult,
    responses: EndpointResult,
}

#[derive(Debug)]
struct EndpointResult {
    ok: bool,
    error: Option<String>,
}

fn probe_model(
    client: &reqwest::blocking::Client,
    api_key: &str,
    model_id: &str,
    base_url: &str,
) -> Result<ProbeResult> {
    let completions = probe_endpoint(
        client,
        api_key,
        base_url,
        "chat/completions",
        json!({
            "model": model_id,
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 5,
            "tools": [{
                "type": "function",
                "function": {
                    "name": "test",
                    "description": "test",
                    "parameters": {
                        "type": "object",
                        "properties": {"x": {"type": "string"}},
                        "required": ["x"]
                    }
                }
            }]
        }),
    )?;

    let responses = probe_endpoint(
        client,
        api_key,
        base_url,
        "responses",
        json!({
            "model": model_id,
            "input": [{"role": "user", "content": "Hi"}],
            "max_output_tokens": 5,
        }),
    )?;

    Ok(ProbeResult {
        completions,
        responses,
    })
}

fn probe_endpoint(
    client: &reqwest::blocking::Client,
    api_key: &str,
    base_url: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<EndpointResult> {
    let url = format!("{base_url}/{path}");
    let response = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .context("调用探测 API 失败")?;

    let status = response.status();
    let text = response.text().context("读取探测 API 响应失败")?;
    let json: Option<serde_json::Value> = serde_json::from_str(&text).ok();

    if status.is_success() {
        if let Some(json) = json {
            if let Some(error) = json.get("error") {
                let message = error
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                return Ok(EndpointResult {
                    ok: false,
                    error: Some(message),
                });
            }
        }
        return Ok(EndpointResult {
            ok: true,
            error: None,
        });
    }

    let error = json
        .and_then(|json| {
            json.get("error")
                .and_then(|value| value.get("message"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));

    Ok(EndpointResult {
        ok: false,
        error: Some(error),
    })
}

// ══════════════════════════════════════════════════
// TUI
// ══════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Agent,
    Provider,
    Model,
}

struct AppState {
    step: Step,
    agent_hint: Option<String>,
    agent_index: usize,
    provider_index: usize,
    model_index: usize,
    selected_agent_id: String,
    config: CxConfig,
}

impl AppState {
    fn new(agent_hint: Option<String>, config: &CxConfig) -> Self {
        let first_agent = resolved_agents(config)
            .first()
            .map(|a| a.id.clone())
            .unwrap_or("copilot".to_string());
        let selected_agent_id = agent_hint.as_ref().cloned().unwrap_or(first_agent);
        let step = if agent_hint.is_some() {
            Step::Provider
        } else {
            Step::Agent
        };

        Self {
            step,
            agent_hint,
            agent_index: 0,
            provider_index: 0,
            model_index: 0,
            selected_agent_id,
            config: config.clone(),
        }
    }

    fn resolved_agents(&self) -> Vec<ResolvedAgent> {
        resolved_agents(&self.config)
    }

    fn current_index(&self) -> usize {
        match self.step {
            Step::Agent => self.agent_index,
            Step::Provider => self.provider_index,
            Step::Model => self.model_index,
        }
    }

    fn current_items(&self, models: &[ResolvedModel]) -> Vec<String> {
        match self.step {
            Step::Agent => self
                .resolved_agents()
                .iter()
                .map(|a| {
                    let mut title = a.id.clone();
                    if let Some(first) = title.get_mut(0..1) {
                        first.make_ascii_uppercase();
                    }
                    title
                })
                .collect(),
            Step::Provider => providers_for_agent(&self.config, &self.selected_agent_id)
                .iter()
                .map(|p| p.name.clone())
                .collect(),
            Step::Model => self
                .current_models(models)
                .iter()
                .map(ResolvedModel::formatted_row)
                .collect(),
        }
    }

    fn current_models(&self, models: &[ResolvedModel]) -> Vec<ResolvedModel> {
        let providers = providers_for_agent(&self.config, &self.selected_agent_id);
        let provider = &providers[self.provider_index];
        models_for_provider(models, &self.selected_agent_id, &provider.name)
    }

    fn move_up(&mut self, models: &[ResolvedModel]) {
        let len = self.current_items(models).len();
        if len == 0 {
            return;
        }
        match self.step {
            Step::Agent => {
                self.agent_index = if self.agent_index == 0 {
                    len - 1
                } else {
                    self.agent_index - 1
                }
            }
            Step::Provider => {
                self.provider_index = if self.provider_index == 0 {
                    len - 1
                } else {
                    self.provider_index - 1
                }
            }
            Step::Model => {
                self.model_index = if self.model_index == 0 {
                    len - 1
                } else {
                    self.model_index - 1
                }
            }
        }
    }

    fn move_down(&mut self, models: &[ResolvedModel]) {
        let len = self.current_items(models).len();
        if len == 0 {
            return;
        }
        match self.step {
            Step::Agent => self.agent_index = (self.agent_index + 1) % len,
            Step::Provider => self.provider_index = (self.provider_index + 1) % len,
            Step::Model => self.model_index = (self.model_index + 1) % len,
        }
    }

    fn confirm(&mut self, models: &[ResolvedModel]) -> Option<Selection> {
        match self.step {
            Step::Agent => {
                let agents = self.resolved_agents();
                if self.agent_index >= agents.len() {
                    return None;
                }
                let agent = &agents[self.agent_index];
                self.selected_agent_id = agent.id.clone();
                self.provider_index = 0;
                self.model_index = 0;
                self.step = Step::Provider;
                None
            }
            Step::Provider => {
                let providers = providers_for_agent(&self.config, &self.selected_agent_id);
                let provider = providers[self.provider_index].clone();
                if provider.requires_model() {
                    self.model_index = 0;
                    self.step = Step::Model;
                    None
                } else {
                    let agent = find_agent(&self.config, &self.selected_agent_id).unwrap();
                    Some(Selection {
                        agent_id: agent.id.clone(),
                        agent_binary: agent.binary.clone(),
                        provider,
                        model: None,
                    })
                }
            }
            Step::Model => {
                let providers = providers_for_agent(&self.config, &self.selected_agent_id);
                let provider = providers[self.provider_index].clone();
                let available = self.current_models(models);
                if self.model_index >= available.len() {
                    return None;
                }
                let agent = find_agent(&self.config, &self.selected_agent_id).unwrap();
                Some(Selection {
                    agent_id: agent.id.clone(),
                    agent_binary: agent.binary.clone(),
                    provider,
                    model: Some(available[self.model_index].clone()),
                })
            }
        }
    }

    fn go_back(&mut self) -> bool {
        match self.step {
            Step::Agent => true,
            Step::Provider => {
                if self.agent_hint.is_some() {
                    true
                } else {
                    self.step = Step::Agent;
                    false
                }
            }
            Step::Model => {
                self.step = Step::Provider;
                false
            }
        }
    }
}

fn run_tui(
    agent_hint: Option<String>,
    config: &CxConfig,
    models: &[ResolvedModel],
) -> Result<Option<Selection>> {
    enable_raw_mode().context("启用终端 raw mode 失败")?;
    let _terminal_guard = TerminalGuard;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("进入备用屏幕失败")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("初始化终端失败")?;

    let mut state = AppState::new(agent_hint, config);

    loop {
        terminal
            .draw(|frame| render(frame, &state, models))
            .context("绘制 TUI 失败")?;

        if let Event::Key(key) = event::read().context("读取终端事件失败")? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Up | KeyCode::Char('k') => state.move_up(models),
                KeyCode::Down | KeyCode::Char('j') => state.move_down(models),
                KeyCode::Enter => {
                    if let Some(selection) = state.confirm(models) {
                        return Ok(Some(selection));
                    }
                }
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                    if state.go_back() {
                        return Ok(None);
                    }
                }
                KeyCode::Char('q') => return Ok(None),
                _ => {}
            }
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

fn render(frame: &mut Frame<'_>, state: &AppState, models: &[ResolvedModel]) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new("cx")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("统一 Agent 入口"),
        );
    frame.render_widget(title, layout[0]);

    let subtitle = Paragraph::new(current_prompt(state))
        .style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: true });
    frame.render_widget(subtitle, layout[1]);

    let items = state.current_items(models);
    let list_items = items
        .iter()
        .map(|item| ListItem::new(item.clone()))
        .collect::<Vec<_>>();
    let mut list_state = ListState::default().with_selected(Some(state.current_index()));
    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(current_title(state)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("✨ ");
    frame.render_stateful_widget(list, layout[2], &mut list_state);

    let footer = Paragraph::new("↑/↓ 或 j/k 移动  ·  Enter 确认  ·  Esc/Backspace 返回  ·  q 退出")
        .style(Style::default().fg(Color::DarkGray))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, layout[3]);
}

fn current_title(state: &AppState) -> &'static str {
    match state.step {
        Step::Agent => "选择 Agent",
        Step::Provider => "选择 Provider",
        Step::Model => "选择 Model",
    }
}

fn current_prompt(state: &AppState) -> String {
    match state.step {
        Step::Agent => "选择 Agent".to_string(),
        Step::Provider => "选择 Provider".to_string(),
        Step::Model => "选择 Model".to_string(),
    }
}

// ══════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_test_config() -> CxConfig {
        CxConfig {
            providers: vec![ProviderConfig {
                name: "Test".into(),
                apikey_source: Some("literal:test".into()),
                agents: vec![],
                models: BTreeMap::new(),
                endpoints: BTreeMap::new(),
            }],
            agents: vec![
                AgentConfig {
                    id: "copilot".into(),
                    binary: "copilot".into(),
                    wire_apis: vec![],
                },
                AgentConfig {
                    id: "claude".into(),
                    binary: "claude".into(),
                    wire_apis: vec![],
                },
                AgentConfig {
                    id: "codex".into(),
                    binary: "codex".into(),
                    wire_apis: vec![],
                },
            ],
        }
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!("cx-{label}-{}", random_urlsafe(6)))
    }

    fn test_resolved_model(model_id: &str, endpoint_url: &str, wire_api: WireApi) -> ResolvedModel {
        ResolvedModel {
            id: model_id.into(),
            arena: "—".into(),
            swe_p: "—".into(),
            tb2: "—".into(),
            desc: String::new(),
            wire_api,
            provider_name: "DashScope".into(),
            endpoint_url: endpoint_url.into(),
            visible_agents: vec!["codex".into(), "claude".into()],
            copilot_auth: CopilotAuth::ApiKey,
        }
    }

    // ── clap CLI parsing tests ──

    fn parse(args: &[&str]) -> Option<CxCommand> {
        Cli::try_parse_from(std::iter::once("cx").chain(args.iter().copied()))
            .ok()
            .and_then(|cli| cli.command)
    }

    #[test]
    fn clap_parse_login() {
        assert_eq!(parse(&["login"]), Some(CxCommand::Login));
    }

    #[test]
    fn clap_parse_logout() {
        assert_eq!(parse(&["logout"]), Some(CxCommand::Logout));
    }

    #[test]
    fn clap_parse_whoami() {
        assert_eq!(parse(&["whoami"]), Some(CxCommand::Whoami));
    }

    #[test]
    fn clap_parse_help() {
        assert_eq!(parse(&["help"]), Some(CxCommand::Help));
    }

    #[test]
    fn clap_parse_probe_no_model() {
        assert_eq!(parse(&["probe"]), Some(CxCommand::Probe { model_id: None }));
    }

    #[test]
    fn clap_parse_probe_with_model() {
        assert_eq!(
            parse(&["probe", "qwen3.6-plus"]),
            Some(CxCommand::Probe {
                model_id: Some("qwen3.6-plus".into())
            })
        );
    }

    #[test]
    fn clap_parse_patch_url() {
        assert_eq!(
            parse(&["patch", "--url", "https://example.com/p.yaml"]),
            Some(CxCommand::Patch {
                url: Some("https://example.com/p.yaml".into()),
                refresh: false
            })
        );
    }

    #[test]
    fn clap_parse_patch_refresh() {
        assert_eq!(
            parse(&["patch", "--refresh"]),
            Some(CxCommand::Patch {
                url: None,
                refresh: true
            })
        );
    }

    #[test]
    fn clap_unknown_subcommand_falls_through() {
        assert!(parse(&["claude", "mcp", "list"]).is_none());
        assert!(parse(&["unknown-cmd"]).is_none());
    }

    // ── Launch spec tests ──

    #[test]
    fn codex_mcp_passthrough_stays_raw_without_endpoints() {
        let selection = Selection {
            agent_id: "codex".into(),
            agent_binary: "codex".into(),
            provider: ResolvedProvider {
                name: "Codex Default".into(),
                has_endpoints: false,
                apikey_source: None,
            },
            model: None,
        };

        let spec = build_launch_spec(&selection, &["mcp".into(), "serve".into()]).unwrap();
        assert_eq!(spec.args, vec!["mcp".to_string(), "serve".to_string()]);
    }

    #[test]
    fn claude_launch_removes_anthropic_env_vars() {
        let selection = Selection {
            agent_id: "claude".into(),
            agent_binary: "claude".into(),
            provider: ResolvedProvider {
                name: "Test".into(),
                has_endpoints: false,
                apikey_source: Some("literal:test-key".into()),
            },
            model: None,
        };
        let spec = build_launch_spec(&selection, &[]).unwrap();
        assert!(spec.env_remove.contains(&"ANTHROPIC_API_KEY".to_string()));
        assert!(
            spec.env_remove
                .contains(&"ANTHROPIC_AUTH_TOKEN".to_string())
        );
        let fake_home = PathBuf::from(spec.env.get("HOME").unwrap());
        let settings: Value = serde_json::from_str(
            &fs::read_to_string(fake_home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "test-key");
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "test-key");
        let _ = fs::remove_dir_all(fake_home);
    }

    #[test]
    fn claude_with_endpoint_removes_anthropic_env_vars() {
        let selection = Selection {
            agent_id: "claude".into(),
            agent_binary: "claude".into(),
            provider: ResolvedProvider {
                name: "DashScope".into(),
                has_endpoints: true,
                apikey_source: Some("literal:test-key".into()),
            },
            model: Some(ResolvedModel {
                id: "qwen3.6-plus".into(),
                arena: "—".into(),
                swe_p: "—".into(),
                tb2: "—".into(),
                desc: String::new(),
                wire_api: WireApi::Anthropic,
                provider_name: "DashScope".into(),
                endpoint_url: "https://dashscope.aliyuncs.com/apps/anthropic".into(),
                visible_agents: vec!["claude".into()],
                copilot_auth: CopilotAuth::ApiKey,
            }),
        };
        let spec = build_launch_spec(&selection, &[]).unwrap();
        assert!(spec.env_remove.contains(&"ANTHROPIC_API_KEY".to_string()));
        assert!(
            spec.env_remove
                .contains(&"ANTHROPIC_AUTH_TOKEN".to_string())
        );
        let fake_home = PathBuf::from(spec.env.get("HOME").unwrap());
        let settings: Value = serde_json::from_str(
            &fs::read_to_string(fake_home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "https://dashscope.aliyuncs.com/apps/anthropic"
        );
        assert_eq!(settings["env"]["ANTHROPIC_API_KEY"], "test-key");
        assert_eq!(settings["model"], "qwen3.6-plus");
        let _ = fs::remove_dir_all(fake_home);
    }

    // ── Merge tests ──

    #[test]
    fn merge_providers_replaces_by_name() {
        let existing = vec![ProviderConfig {
            name: "A".into(),
            apikey_source: Some("literal:old".into()),
            agents: vec![],
            models: BTreeMap::new(),
            endpoints: BTreeMap::new(),
        }];
        let incoming = vec![ProviderConfig {
            name: "A".into(),
            apikey_source: Some("literal:new".into()),
            agents: vec![],
            models: BTreeMap::new(),
            endpoints: BTreeMap::new(),
        }];
        let merged = merge_providers(&existing, &incoming);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].apikey_source.as_deref(), Some("literal:new"));
    }

    #[test]
    fn merge_providers_appends_new() {
        let existing = vec![ProviderConfig {
            name: "A".into(),
            apikey_source: None,
            agents: vec![],
            models: BTreeMap::new(),
            endpoints: BTreeMap::new(),
        }];
        let incoming = vec![ProviderConfig {
            name: "B".into(),
            apikey_source: None,
            agents: vec![],
            models: BTreeMap::new(),
            endpoints: BTreeMap::new(),
        }];
        let merged = merge_providers(&existing, &incoming);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_providers_preserves_order_for_replacements() {
        let existing = vec![
            ProviderConfig {
                name: "A".into(),
                apikey_source: Some("literal:a".into()),
                agents: vec![],
                models: BTreeMap::new(),
                endpoints: BTreeMap::new(),
            },
            ProviderConfig {
                name: "B".into(),
                apikey_source: Some("literal:old".into()),
                agents: vec![],
                models: BTreeMap::new(),
                endpoints: BTreeMap::new(),
            },
            ProviderConfig {
                name: "C".into(),
                apikey_source: Some("literal:c".into()),
                agents: vec![],
                models: BTreeMap::new(),
                endpoints: BTreeMap::new(),
            },
        ];
        let incoming = vec![
            ProviderConfig {
                name: "B".into(),
                apikey_source: Some("literal:new".into()),
                agents: vec![],
                models: BTreeMap::new(),
                endpoints: BTreeMap::new(),
            },
            ProviderConfig {
                name: "D".into(),
                apikey_source: Some("literal:d".into()),
                agents: vec![],
                models: BTreeMap::new(),
                endpoints: BTreeMap::new(),
            },
        ];

        let merged = merge_providers(&existing, &incoming);
        let names: Vec<&str> = merged
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["A", "B", "C", "D"]);
        assert_eq!(merged[1].apikey_source.as_deref(), Some("literal:new"));
    }

    #[test]
    fn merge_agents_replaces_by_id() {
        let existing = vec![AgentConfig {
            id: "claude".into(),
            binary: "claude-old".into(),
            wire_apis: vec![],
        }];
        let incoming = vec![AgentConfig {
            id: "claude".into(),
            binary: "claude-new".into(),
            wire_apis: vec![],
        }];
        let merged = merge_agents(&existing, &incoming);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].binary, "claude-new");
    }

    #[test]
    fn merge_agents_appends_new() {
        let existing = vec![AgentConfig {
            id: "copilot".into(),
            binary: "copilot".into(),
            wire_apis: vec![],
        }];
        let incoming = vec![AgentConfig {
            id: "codex".into(),
            binary: "codex".into(),
            wire_apis: vec![],
        }];
        let merged = merge_agents(&existing, &incoming);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_agents_preserves_order_for_replacements() {
        let existing = vec![
            AgentConfig {
                id: "copilot".into(),
                binary: "copilot".into(),
                wire_apis: vec![],
            },
            AgentConfig {
                id: "claude".into(),
                binary: "claude-old".into(),
                wire_apis: vec![],
            },
            AgentConfig {
                id: "codex".into(),
                binary: "codex".into(),
                wire_apis: vec![],
            },
        ];
        let incoming = vec![
            AgentConfig {
                id: "claude".into(),
                binary: "claude-new".into(),
                wire_apis: vec![],
            },
            AgentConfig {
                id: "gemini".into(),
                binary: "gemini".into(),
                wire_apis: vec![],
            },
        ];

        let merged = merge_agents(&existing, &incoming);
        let ids: Vec<&str> = merged.iter().map(|agent| agent.id.as_str()).collect();
        assert_eq!(ids, vec!["copilot", "claude", "codex", "gemini"]);
        assert_eq!(merged[1].binary, "claude-new");
    }

    #[test]
    fn merge_codex_config_rewrites_provider_and_project_section() {
        let existing = r#"
model = "old-model"
model_provider = "old-provider"
approval_policy = "on-request"

[model_providers.dashscope]
name = "Old"
base_url = "https://old.example.com"
env_key = "OLD_KEY"
wire_api = "responses"

[projects."/tmp/workspace"]
trust_level = "untrusted"

[projects."/tmp/other"]
trust_level = "trusted"
"#;

        let merged = merge_codex_config(
            Some(existing),
            &test_resolved_model(
                "qwen3.6-plus",
                "https://dashscope.aliyuncs.com/v1",
                WireApi::Responses,
            ),
            Path::new("/tmp/workspace"),
        )
        .unwrap();

        assert!(merged.contains(r#"model = "qwen3.6-plus""#));
        assert!(merged.contains(r#"base_url = "https://dashscope.aliyuncs.com/v1""#));
        assert!(merged.contains(r#"[projects."/tmp/workspace"]"#));
        assert!(merged.contains(r#"trust_level = "trusted""#));
        assert!(merged.contains(r#"approval_policy = "on-request""#));
        assert!(merged.contains(r#"[projects."/tmp/other"]"#));
        assert!(!merged.contains("https://old.example.com"));
    }

    #[test]
    fn merge_claude_settings_preserves_existing_keys_and_overrides_env() {
        let existing = r#"{
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  },
  "model": "opus[1m]",
  "theme": "light"
}"#;
        let mut env_overrides = BTreeMap::new();
        env_overrides.insert(
            "ANTHROPIC_BASE_URL".into(),
            "https://dashscope.aliyuncs.com/apps/anthropic".into(),
        );
        env_overrides.insert("ANTHROPIC_API_KEY".into(), "test-key".into());

        let merged =
            merge_claude_settings(Some(existing), &env_overrides, Some("qwen3.6-plus")).unwrap();
        let merged: Value = serde_json::from_str(&merged).unwrap();

        assert_eq!(merged["theme"], "light");
        assert_eq!(merged["model"], "qwen3.6-plus");
        assert_eq!(merged["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1");
        assert_eq!(
            merged["env"]["ANTHROPIC_BASE_URL"],
            "https://dashscope.aliyuncs.com/apps/anthropic"
        );
        assert_eq!(merged["env"]["ANTHROPIC_API_KEY"], "test-key");
    }

    #[test]
    fn materialize_passthrough_dir_skips_overridden_entries() {
        let root = temp_test_dir("passthrough-dir");
        let real = root.join("real");
        let fake = root.join("fake");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("keep.txt"), "ok").unwrap();
        fs::write(real.join("override.txt"), "skip").unwrap();

        materialize_passthrough_dir(&real, &fake, &["override.txt"]).unwrap();

        assert!(fake.join("keep.txt").exists());
        assert!(!fake.join("override.txt").exists());
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(fake.join("keep.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_legacy_provider_config_moves_file() {
        let dir = temp_test_dir("provider-config-migration");
        fs::create_dir_all(&dir).unwrap();

        let current_path = dir.join(PROVIDER_CONFIG_FILE_NAME);
        let legacy_path = dir.join(LEGACY_PROVIDER_CONFIG_FILE_NAME);
        let content = "providers: []\nagents: []\n";
        fs::write(&legacy_path, content).unwrap();

        let migrated = migrate_legacy_provider_config(&current_path, &legacy_path).unwrap();
        assert!(migrated);
        assert!(!legacy_path.exists());
        assert_eq!(fs::read_to_string(&current_path).unwrap(), content);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolved_agents_hide_legacy_codex_app_entry() {
        let config = CxConfig {
            providers: vec![],
            agents: vec![
                AgentConfig {
                    id: "codex".into(),
                    binary: "codex".into(),
                    wire_apis: vec![],
                },
                AgentConfig {
                    id: "codex-app".into(),
                    binary: "codex-app".into(),
                    wire_apis: vec![],
                },
            ],
        };
        let agents = resolved_agents(&config);
        assert_eq!(agents.iter().filter(|agent| agent.id == "codex").count(), 1);
        assert!(agents.iter().all(|agent| agent.id != "codex-app"));
    }

    #[test]
    fn provider_lists_end_with_add_provider() {
        let config = minimal_test_config();
        for agent in resolved_agents(&config) {
            let providers = providers_for_agent(&config, &agent.id);
            assert_eq!(
                providers.last().map(|p| p.name.as_str()),
                Some("+ 添加 Provider")
            );
        }
    }

    #[test]
    fn resolve_binary_finds_codex_cli() {
        let path = resolve_binary("codex").unwrap();
        assert!(path.ends_with("codex"));
    }
}
