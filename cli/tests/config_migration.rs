//! The public rename must preserve an existing local configuration without overwriting data.

use std::fs;
use stoic_codex_cli::{run, Ctx, OK};

#[test]
fn first_command_moves_the_previous_product_config_and_reports_it() {
    let home = std::env::temp_dir().join(format!(
        "stoic-codex-config-migration-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    let legacy = home.join(".zenithal");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("config.json"), r#"{"memory":{"backend":"file"}}"#).unwrap();

    let original_new = std::env::var_os("STOIC_CODEX_HOME");
    let original_legacy = std::env::var_os("ZENITHAL_HOME");
    std::env::set_var("STOIC_CODEX_HOME", home.as_os_str());
    std::env::remove_var("ZENITHAL_HOME");

    let (mut ctx, buffers) = Ctx::buffered(home.clone());
    let code = run(&["tools".to_string()], &mut ctx);
    let error_output = buffers.err();

    match original_new {
        Some(value) => std::env::set_var("STOIC_CODEX_HOME", value),
        None => std::env::remove_var("STOIC_CODEX_HOME"),
    }
    match original_legacy {
        Some(value) => std::env::set_var("ZENITHAL_HOME", value),
        None => std::env::remove_var("ZENITHAL_HOME"),
    }

    let current = home.join(".stoic-codex");
    assert_eq!(code, OK);
    assert!(current.join("config.json").is_file());
    assert!(!legacy.exists());
    assert!(error_output.contains("migrated local config"), "{error_output}");

    let _ = fs::remove_dir_all(home);
}
