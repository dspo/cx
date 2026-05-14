use std::env;
use std::fs;
use std::path::Path;
const DEFAULT_GITLAB_BASE_URL: &str = "https://git.huayi.tech";
const DEFAULT_GITLAB_CLIENT_ID: &str =
    "738f804d33e6eb3d2c819e335cbeb65a70d2818b1da7e09fd2d5774630b51fc4";
const DEFAULT_GITLAB_CALLBACK_URL: &str = "http://127.0.0.1:38081/callback";
const DEFAULT_GITLAB_SCOPES: &str = "read_user openid profile email";

fn main() {
    println!("cargo:rerun-if-env-changed=CX_GITLAB_BASE_URL");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_CALLBACK_URL");
    println!("cargo:rerun-if-env-changed=CX_GITLAB_SCOPES");

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
        "pub const GITLAB_BASE_URL: &str = {gitlab_base_url:?};\n\
         pub const GITLAB_CLIENT_ID: &str = {gitlab_client_id:?};\n\
         pub const GITLAB_CALLBACK_URL: &str = {gitlab_callback_url:?};\n\
         pub const GITLAB_SCOPES: &str = {gitlab_scopes:?};\n"
    );
    fs::write(&generated_path, generated)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", generated_path.display()));
}
