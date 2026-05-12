use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

const CONFIG_TEMPLATE_PATH: &str = "config/internal.config.yaml";
const DEFAULT_GITLAB_BASE_URL: &str = "https://git.huayi.tech";
const DEFAULT_GITLAB_CLIENT_ID: &str =
    "738f804d33e6eb3d2c819e335cbeb65a70d2818b1da7e09fd2d5774630b51fc4";
const DEFAULT_GITLAB_CALLBACK_URL: &str = "http://127.0.0.1:38081/callback";
const DEFAULT_GITLAB_SCOPES: &str = "read_user openid profile email";

const TEMPLATE_VARS: [(&str, &str); 3] = [
    ("CX_DASHSCOPE_API_KEY", "dev-dashscope-key"),
    ("CX_ANTHROPIC_API_KEY", "dev-anthropic-key"),
    ("CX_MIMO_API_KEY", "dev-mimo-api-key"),
];

fn main() {
    println!("cargo:rerun-if-changed={CONFIG_TEMPLATE_PATH}");
    println!("cargo:rerun-if-env-changed=CX_ENFORCE_EMBEDDED_SECRETS");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_BASE_URL");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_CALLBACK_URL");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_SCOPES");

    let mut template_vars = BTreeMap::new();
    let mut missing = Vec::new();
    for (key, default) in TEMPLATE_VARS {
        println!("cargo:rerun-if-env-changed={key}");
        match env::var(key) {
            Ok(value) => {
                template_vars.insert(key, yaml_single_quote_escape(&value));
            }
            Err(_) => {
                template_vars.insert(key, yaml_single_quote_escape(default));
                missing.push(key);
            }
        }
    }

    let enforce_secrets = env_flag("CX_ENFORCE_EMBEDDED_SECRETS");
    if enforce_secrets && !missing.is_empty() {
        panic!(
            "missing embedded secret env vars for release build: {}",
            missing.join(", ")
        );
    }

    if !missing.is_empty() {
        println!(
            "cargo:warning=using development placeholder values for: {}",
            missing.join(", ")
        );
    }

    let template = fs::read_to_string(CONFIG_TEMPLATE_PATH)
        .unwrap_or_else(|err| panic!("failed to read {CONFIG_TEMPLATE_PATH}: {err}"));
    let embedded_config_yaml = render_template(&template, &template_vars);

    let gitlab_base_url =
        env::var("CX_GITLAB_BASE_URL").unwrap_or_else(|_| DEFAULT_GITLAB_BASE_URL.to_string());
    let gitlab_client_id =
        env::var("CX_GITLAB_CLIENT_ID").unwrap_or_else(|_| DEFAULT_GITLAB_CLIENT_ID.to_string());
    let gitlab_callback_url = env::var("CX_GITLAB_CALLBACK_URL")
        .unwrap_or_else(|_| DEFAULT_GITLAB_CALLBACK_URL.to_string());
    let gitlab_scopes =
        env::var("CX_GITLAB_SCOPES").unwrap_or_else(|_| DEFAULT_GITLAB_SCOPES.to_string());

    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR is not set");
    let generated_path = Path::new(&out_dir).join("embedded_config.rs");
    let generated = format!(
        "pub const EMBEDDED_CONFIG_YAML: &str = r####\"{embedded_config_yaml}\"####;\n\
         pub const GITLAB_BASE_URL: &str = {gitlab_base_url:?};\n\
         pub const GITLAB_CLIENT_ID: &str = {gitlab_client_id:?};\n\
         pub const GITLAB_CALLBACK_URL: &str = {gitlab_callback_url:?};\n\
         pub const GITLAB_SCOPES: &str = {gitlab_scopes:?};\n"
    );
    fs::write(&generated_path, generated)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", generated_path.display()));
}

fn render_template(template: &str, values: &BTreeMap<&str, String>) -> String {
    let mut rendered = template.to_string();
    for (key, value) in values {
        rendered = rendered.replace(&format!("${{{key}}}"), value);
    }
    rendered
}

fn yaml_single_quote_escape(value: &str) -> String {
    value.replace('\'', "''")
}

fn env_flag(key: &str) -> bool {
    matches!(
        env::var(key).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES")
    )
}
