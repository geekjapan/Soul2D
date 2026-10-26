//! The unsaved-changes dialog (port of `io/save.d` `CloseAskHandler.show` and the pending-action dialog of
//! `panels/rustcore.d`): Save / Don't Save / Cancel, with a path field that Save fills in for a document
//! that has no path yet.

use eframe::egui;
use soul2d_core::window_shell::Choice;

/// What the dialog is holding while it is open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnsavedDialog {
    /// The path Save writes to; prefilled with the document's own path.
    pub path: String,
    /// Why the last Save did not finish (no path, a write error, ...).
    pub message: String,
    /// The path field takes keyboard focus on the next frame.
    pub focus_path: bool,
}

impl UnsavedDialog {
    pub fn new(project_path: Option<&str>) -> Self {
        Self { path: project_path.unwrap_or_default().to_owned(), message: String::new(), focus_path: project_path.is_none() }
    }
}

/// Draws the dialog. `Some(Choice::Save)` carries the typed path through `dialog.path`.
pub fn unsaved_dialog(ctx: &egui::Context, dialog: &mut UnsavedDialog, has_path: bool, tr: &dyn Fn(&str) -> String) -> Option<Choice> {
    let mut choice = None;
    egui::Window::new(tr("Save before closing?"))
        .id(egui::Id::new("editor_shell_unsaved"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(tr("Would you like to save your changes before closing?\n\nYou can change the default behaviour in the settings."));
            if !has_path && dialog.path.trim().is_empty() {
                ui.label(tr("Enter a save path below to continue."));
            }
            ui.horizontal(|ui| {
                let caption = ui.label(tr("Save path"));
                let field = ui.add(egui::TextEdit::singleline(&mut dialog.path).desired_width(320.0));
                field.clone().labelled_by(caption.id);
                if std::mem::take(&mut dialog.focus_path) {
                    field.request_focus();
                }
                if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    choice = Some(Choice::Save);
                }
                if ui.button(tr("Browse...")).clicked() {
                    if let Some(path) = crate::editor_windows::browse(crate::editor_windows::PromptKind::SaveAs, &dialog.path) {
                        dialog.path = path;
                    }
                }
            });
            if !dialog.message.is_empty() {
                ui.colored_label(egui::Color32::LIGHT_RED, dialog.message.as_str());
            }
            ui.horizontal(|ui| {
                if ui.button(tr("Save")).clicked() {
                    choice = Some(Choice::Save);
                }
                if ui.button(tr("Don't Save")).clicked() {
                    choice = Some(Choice::DontSave);
                }
                if ui.button(tr("Cancel")).clicked() {
                    choice = Some(Choice::Cancel);
                }
            });
        });
    // Escape cancels like the dialog's Cancel button.
    if choice.is_none() && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        choice = Some(Choice::Cancel);
    }
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_test_support::{assert_all_named, has, texts, Harness};
    use egui::accesskit::Role;

    fn tr(msgid: &str) -> String {
        msgid.to_owned()
    }

    fn answer_of(label: &str, mut dialog: UnsavedDialog, has_path: bool) -> (Option<Choice>, UnsavedDialog) {
        let mut harness = Harness::new();
        let mut answer = None;
        {
            let mut run = |ctx: &egui::Context| {
                if let Some(choice) = unsaved_dialog(ctx, &mut dialog, has_path, &tr) {
                    answer = Some(choice);
                }
            };
            harness.click(Role::Button, label, &mut run);
        }
        (answer, dialog)
    }

    #[test]
    fn the_dialog_offers_save_dont_save_and_cancel_with_named_controls() {
        let mut harness = Harness::new();
        let mut dialog = UnsavedDialog::new(None);
        let output = harness.frame(Vec::new(), |ctx| {
            unsaved_dialog(ctx, &mut dialog, false, &tr);
        });
        let output = harness.frame(Vec::new(), |ctx| {
            unsaved_dialog(ctx, &mut dialog, false, &tr);
        }).into_iter().chain(output).collect();
        assert!(assert_all_named(&output, "unsaved dialog") >= 4, "path field and three buttons");
        for button in ["Save", "Don't Save", "Cancel"] {
            assert!(has(&output, Role::Button, button), "{button}");
        }
        assert!(has(&output, Role::TextInput, "Save path"));
        assert!(texts(&output).iter().any(|t| t == "Enter a save path below to continue."), "{:?}", texts(&output));
    }

    #[test]
    fn each_button_gives_its_choice() {
        for (label, expected) in [("Save", Choice::Save), ("Don't Save", Choice::DontSave), ("Cancel", Choice::Cancel)] {
            let (answer, _) = answer_of(label, UnsavedDialog::new(Some("/p/a.inx")), true);
            assert_eq!(answer, Some(expected), "{label}");
        }
    }

    #[test]
    fn the_path_field_starts_with_the_documents_path_and_a_message_is_shown() {
        let dialog = UnsavedDialog::new(Some("/p/a.inx"));
        assert_eq!((dialog.path.as_str(), dialog.focus_path), ("/p/a.inx", false));
        let untitled = UnsavedDialog::new(None);
        assert_eq!((untitled.path.as_str(), untitled.focus_path), ("", true), "an untitled document asks for a path");
        let mut harness = Harness::new();
        let mut shown = UnsavedDialog { message: "disk full".into(), ..UnsavedDialog::new(None) };
        harness.frame(Vec::new(), |ctx| {
            unsaved_dialog(ctx, &mut shown, false, &tr);
        });
        let output = harness.frame(Vec::new(), |ctx| {
            unsaved_dialog(ctx, &mut shown, false, &tr);
        });
        assert!(texts(&output).iter().any(|t| t == "disk full"));
    }

    #[test]
    fn escape_cancels() {
        let mut harness = Harness::new();
        let mut dialog = UnsavedDialog::new(Some("/p/a.inx"));
        harness.frame(Vec::new(), |ctx| {
            unsaved_dialog(ctx, &mut dialog, true, &tr);
        });
        let mut answer = None;
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        harness.frame(vec![escape], |ctx| answer = unsaved_dialog(ctx, &mut dialog, true, &tr));
        assert_eq!(answer, Some(Choice::Cancel));
    }
}
