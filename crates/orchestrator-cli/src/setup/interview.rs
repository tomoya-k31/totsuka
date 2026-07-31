//! Prompting primitives for `totsuka setup` (#348).
//!
//! Reader and writer are injected rather than reaching for `stdin`/`stdout`
//! directly, so the whole interview can be driven from a test with scripted
//! answers. That matters more than usual here: the wizard is the one command
//! that cannot be exercised end-to-end through the CLI binary (a child process
//! has no terminal), so if the prompting logic is not unit-testable it is not
//! tested at all.

use std::io::{BufRead, Write};

/// Terminal I/O for the interview.
pub struct Prompt<'a> {
    input: &'a mut dyn BufRead,
    output: &'a mut dyn Write,
}

/// End of input while a question was still pending.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// The reader ran out — the user pressed Ctrl-D, or a scripted answer file
    /// was short.
    #[error("input ended while `{question}` was still unanswered")]
    Eof {
        /// The question that went unanswered.
        question: String,
    },
    /// Reading or writing failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl<'a> Prompt<'a> {
    /// Wrap a reader and writer.
    pub fn new(input: &'a mut dyn BufRead, output: &'a mut dyn Write) -> Self {
        Self { input, output }
    }

    /// Print a line (section headers, explanations, the plan).
    pub fn say(&mut self, line: &str) -> Result<(), PromptError> {
        writeln!(self.output, "{line}")?;
        Ok(())
    }

    /// Ask for free text. An empty answer takes `default` when there is one;
    /// with no default, the question is repeated until something is typed —
    /// silently accepting an empty required value would produce a config that
    /// fails to load later, far from the question that caused it.
    pub fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, PromptError> {
        loop {
            match default {
                Some(d) => write!(self.output, "{question} [{d}]: ")?,
                None => write!(self.output, "{question}: ")?,
            }
            self.output.flush()?;
            let answer = self.read_line(question)?;
            let answer = answer.trim();
            if !answer.is_empty() {
                return Ok(answer.to_string());
            }
            match default {
                Some(d) => return Ok(d.to_string()),
                None => writeln!(self.output, "  (required)")?,
            }
        }
    }

    /// Ask to pick one of `choices`, by number. Re-asks on anything that is not
    /// a number in range.
    pub fn choose(
        &mut self,
        question: &str,
        choices: &[(&str, &str)],
        default: usize,
    ) -> Result<usize, PromptError> {
        loop {
            writeln!(self.output, "{question}")?;
            for (index, (label, blurb)) in choices.iter().enumerate() {
                writeln!(self.output, "  {}) {label}", index + 1)?;
                if !blurb.is_empty() {
                    writeln!(self.output, "     {blurb}")?;
                }
            }
            write!(self.output, "Choice [{}]: ", default + 1)?;
            self.output.flush()?;

            let answer = self.read_line(question)?;
            let answer = answer.trim();
            if answer.is_empty() {
                return Ok(default);
            }
            match answer.parse::<usize>() {
                Ok(n) if n >= 1 && n <= choices.len() => return Ok(n - 1),
                _ => writeln!(self.output, "  (enter 1-{})", choices.len())?,
            }
        }
    }

    /// Yes/no. `default` decides what a bare Enter means.
    pub fn confirm(&mut self, question: &str, default: bool) -> Result<bool, PromptError> {
        let hint = if default { "Y/n" } else { "y/N" };
        loop {
            write!(self.output, "{question} [{hint}]: ")?;
            self.output.flush()?;
            let answer = self.read_line(question)?;
            match answer.trim().to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => writeln!(self.output, "  (y or n)")?,
            }
        }
    }

    /// Read one line, turning end-of-input into an error rather than an empty
    /// answer — `""` is a meaningful reply to several of these questions, so
    /// the two must not be conflated.
    fn read_line(&mut self, question: &str) -> Result<String, PromptError> {
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Err(PromptError::Eof {
                question: question.to_string(),
            });
        }
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a prompt with scripted input, returning (result, what was printed).
    fn scripted<T>(
        input: &str,
        f: impl FnOnce(&mut Prompt) -> Result<T, PromptError>,
    ) -> (Result<T, PromptError>, String) {
        let mut reader = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut written: Vec<u8> = Vec::new();
        let result = {
            let mut prompt = Prompt::new(&mut reader, &mut written);
            f(&mut prompt)
        };
        (result, String::from_utf8(written).unwrap())
    }

    #[test]
    fn ask_takes_the_default_on_a_bare_enter() {
        let (answer, shown) = scripted("\n", |p| p.ask("Repository path", Some("~/repo")));
        assert_eq!(answer.unwrap(), "~/repo");
        assert!(shown.contains("[~/repo]"), "default not offered: {shown}");
    }

    #[test]
    fn ask_trims_and_keeps_what_was_typed() {
        let (answer, _) = scripted("  ~/Workspace/totsuka  \n", |p| p.ask("Path", Some("x")));
        assert_eq!(answer.unwrap(), "~/Workspace/totsuka");
    }

    #[test]
    fn ask_without_a_default_repeats_until_answered() {
        let (answer, shown) = scripted("\n\ntotsuka\n", |p| p.ask("Name", None));
        assert_eq!(answer.unwrap(), "totsuka");
        assert_eq!(shown.matches("(required)").count(), 2, "{shown}");
    }

    #[test]
    fn choose_defaults_and_validates_the_range() {
        let choices = [("Minimal", "just enough"), ("Slack", "reply as yourself")];

        let (picked, _) = scripted("\n", |p| p.choose("Recipe", &choices, 0));
        assert_eq!(picked.unwrap(), 0);

        let (picked, _) = scripted("2\n", |p| p.choose("Recipe", &choices, 0));
        assert_eq!(picked.unwrap(), 1);

        // Out of range and non-numeric both re-ask rather than guessing.
        let (picked, shown) = scripted("9\nzero\n1\n", |p| p.choose("Recipe", &choices, 1));
        assert_eq!(picked.unwrap(), 0);
        assert_eq!(shown.matches("(enter 1-2)").count(), 2, "{shown}");
    }

    #[test]
    fn confirm_honors_the_default_and_rejects_noise() {
        let (yes, _) = scripted("\n", |p| p.confirm("Proceed?", true));
        assert!(yes.unwrap());

        let (no, _) = scripted("\n", |p| p.confirm("Proceed?", false));
        assert!(!no.unwrap());

        let (yes, shown) = scripted("maybe\ny\n", |p| p.confirm("Proceed?", false));
        assert!(yes.unwrap());
        assert!(shown.contains("(y or n)"), "{shown}");
    }

    #[test]
    fn end_of_input_is_an_error_not_an_empty_answer() {
        // Ctrl-D must abort the interview. Treating it as "" would silently
        // accept defaults for every remaining question.
        let (answer, _) = scripted("", |p| p.ask("Name", None));
        assert!(matches!(answer, Err(PromptError::Eof { .. })));

        let (answer, _) = scripted("", |p| p.confirm("Proceed?", true));
        assert!(matches!(answer, Err(PromptError::Eof { .. })));
    }
}
