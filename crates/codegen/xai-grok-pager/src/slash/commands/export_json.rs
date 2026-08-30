//! `/export-json [filename]` — dump session stats JSON (tokens, tools, isolation).
//!
//! Pager builtin: never sent to the model. Reads restored `usage.json` after
//! `/resume`. Omit the filename to copy JSON to the clipboard.

use std::path::PathBuf;

use crate::app::actions::Action;
use crate::slash::command::{
    AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand, slash_meta,
};
use crate::slash::commands::export::list_path_completions;

pub struct ExportJsonCommand;

impl SlashCommand for ExportJsonCommand {
    slash_meta! {
        name: "export-json",
        description: "Export session stats (tokens, tools, isolation) as JSON",
        usage: "/export-json [filename]",
        takes_args: true,
        args_required: false,
        session_scoped: true,
        arg_placeholder: "[filename]",
    }

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let items = list_path_completions(ctx.cwd, args_query);
        if items.is_empty() { None } else { Some(items) }
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        if ctx.session_id.is_none() {
            return CommandResult::Error("No active session to export".to_string());
        }

        let trimmed = args.trim();
        let file_path: Option<PathBuf> = if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        };

        CommandResult::Action(Action::ExportJson { file_path })
    }
}

#[cfg(test)]
mod export_json_command_tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::actions::Action;
    use crate::app::bundle::BundleState;
    use crate::settings::PagerLocalSnapshot;

    static DEFAULT_BUNDLE_STATE: BundleState = BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn make_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn export_json_no_session_errors() {
        let models = ModelState::default();
        let mut ctx = make_ctx(&models);
        match ExportJsonCommand.run(&mut ctx, "") {
            CommandResult::Error(msg) => assert!(msg.contains("No active session")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn export_json_does_not_pass_through_as_user_prompt() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::from("test-session".to_string());
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: Some(&sid),
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        match ExportJsonCommand.run(&mut ctx, "") {
            CommandResult::Action(Action::ExportJson { file_path }) => {
                assert!(file_path.is_none());
            }
            CommandResult::PassThrough(_) => {
                panic!("export-json must not enqueue a user prompt")
            }
            other => panic!("expected ExportJson action, got {other:?}"),
        }
    }

    #[test]
    fn export_json_dispatches_file_path_when_given() {
        let models = ModelState::default();
        let sid = agent_client_protocol::SessionId::from("s2".to_string());
        let mut ctx = CommandExecCtx {
            models: &models,
            session_id: Some(&sid),
            bundle_state: &DEFAULT_BUNDLE_STATE,
            screen_mode: crate::app::ScreenMode::Inline,
            billing_surface_visible: true,
            usage_command_visible: true,
            pager_state: PagerLocalSnapshot::default(),
        };
        match ExportJsonCommand.run(&mut ctx, "~/exports/run.json") {
            CommandResult::Action(Action::ExportJson { file_path }) => {
                let p = file_path.expect("some path");
                assert!(p.to_string_lossy().contains("run.json"));
            }
            other => panic!("expected ExportJson(Some), got {other:?}"),
        }
    }

    #[test]
    fn export_json_is_reserved_pager_builtin() {
        assert!(
            xai_grok_shell::session::PAGER_COMMAND_KEYS.contains(&"export-json"),
            "missing export-json from PAGER_COMMAND_KEYS would send the slash text to the model"
        );
        let names: Vec<_> = crate::slash::commands::builtin_commands()
            .iter()
            .map(|c| c.name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "export-json"),
            "export-json must be a pager builtin"
        );
    }
}
