//! Explicit operator entry to the same HookManager used by the desktop UI.
//! Does not grant trust or execute a Codex task; not included in the portable bundle.
use promptdock_desktop_lib::{
    desktop_capture::read_policy,
    hook_installer::{HookFeatures, HookManager},
};
use std::{error::Error, io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.len() != 3 {
        return Err(
            "usage: install_desktop_hook <absolute-exe> <absolute-codex-home> <absolute-app-data>"
                .into(),
        );
    }
    let (executable, home, data) = (&args[0], &args[1], &args[2]);
    if args.iter().any(|path| !path.is_absolute())
        || !executable.is_file()
        || !home.is_dir()
        || !data.is_dir()
    {
        return Err("all paths must exist and be absolute".into());
    }
    let config = home.join("hooks.json");
    if config.exists()
        && String::from_utf8(std::fs::read(&config)?)?.contains("--promptdock-agent-event")
    {
        return Err("legacy PromptDock hooks must be migrated explicitly first".into());
    }
    let target = data.join("hook-target.json");
    if target.exists() {
        if serde_json::from_slice::<PathBuf>(&std::fs::read(&target)?)? != *home {
            return Err("existing Hook target differs; uninstall it before changing scope".into());
        }
    } else {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        file.write_all(&serde_json::to_vec(home)?)?;
        file.sync_all()?;
    }
    let inbox = data.join("agent-events.jsonl");
    let policy = read_policy(&inbox);
    HookManager::new(config, inbox, executable.clone())?.install_features(HookFeatures {
        prompt_capture: policy.observe_turns,
        run_lifecycle: policy.observe_turns,
        attention: policy.observe_turns && policy.notify_attention,
        // The policy snapshot controls excerpts without changing Hook trust identity.
        capture_agent_outputs: false,
    })?;
    println!("Hook configuration installed. Host trust and real events remain unverified.");
    Ok(())
}
