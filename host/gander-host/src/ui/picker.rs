// SPDX-License-Identifier: Apache-2.0
//! Folder pickers: the Windows dialog for the application, fakes for tests.

use std::path::PathBuf;
use std::sync::Mutex;

use super::controller::FolderPicker;

/// The native "choose a folder" dialog (rfd, Windows `IFileDialog`).
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct SystemPicker;

#[cfg(windows)]
impl FolderPicker for SystemPicker {
    fn pick_folder(&self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Choose a folder to share")
            .pick_folder()
    }
}

/// Answers from a list without opening anything: each call pops the next answer, `None` when the
/// list is empty (as if the dialog was cancelled).
#[derive(Debug, Default)]
pub struct ScriptedPicker {
    answers: Mutex<Vec<Option<PathBuf>>>,
}

impl ScriptedPicker {
    /// A picker that returns `answers` in order.
    pub fn new(answers: Vec<Option<PathBuf>>) -> Self {
        let mut answers = answers;
        answers.reverse();
        Self {
            answers: Mutex::new(answers),
        }
    }
}

impl FolderPicker for ScriptedPicker {
    fn pick_folder(&self) -> Option<PathBuf> {
        self.answers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_answers_come_back_in_order_then_cancel() {
        let p = ScriptedPicker::new(vec![
            Some(PathBuf::from("a")),
            None,
            Some(PathBuf::from("b")),
        ]);
        assert_eq!(p.pick_folder(), Some(PathBuf::from("a")));
        assert_eq!(p.pick_folder(), None);
        assert_eq!(p.pick_folder(), Some(PathBuf::from("b")));
        assert_eq!(
            p.pick_folder(),
            None,
            "empty list behaves like a cancelled dialog"
        );
    }
}
