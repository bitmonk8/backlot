use crate::error::FlickError;

/// Abstraction over interactive prompts, mockable in tests.
pub trait Prompter {
    /// Display a password prompt (hidden input). Returns the entered string.
    fn password(&self, prompt: &str) -> Result<String, FlickError>;

    /// Display a selection list. Returns the index of the selected item.
    fn select(&self, prompt: &str, items: &[String], default: usize)
        -> Result<usize, FlickError>;

    /// Display a text input with an optional default.
    /// Returns the entered string (or default if empty).
    fn input(&self, prompt: &str, default: Option<&str>)
        -> Result<String, FlickError>;

    /// Display a yes/no confirmation. Returns true for yes.
    fn confirm(&self, prompt: &str, default: bool) -> Result<bool, FlickError>;

    /// Display a multi-select list. Returns indices of selected items.
    fn multi_select(&self, prompt: &str, items: &[String], defaults: &[bool])
        -> Result<Vec<usize>, FlickError>;

    /// Print a message to the user (stderr).
    fn message(&self, msg: &str) -> Result<(), FlickError>;
}

/// Production prompter wrapping `dialoguer` widgets. All output targets stderr.
pub struct TerminalPrompter {
    term: dialoguer::console::Term,
}

impl Default for TerminalPrompter {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalPrompter {
    pub fn new() -> Self {
        Self {
            term: dialoguer::console::Term::stderr(),
        }
    }
}

impl Prompter for TerminalPrompter {
    fn password(&self, prompt: &str) -> Result<String, FlickError> {
        dialoguer::Password::new()
            .with_prompt(prompt)
            .allow_empty_password(true)
            .interact_on(&self.term)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))
    }

    fn select(
        &self,
        prompt: &str,
        items: &[String],
        default: usize,
    ) -> Result<usize, FlickError> {
        dialoguer::Select::new()
            .with_prompt(prompt)
            .items(items)
            .default(default)
            .interact_on(&self.term)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))
    }

    fn input(&self, prompt: &str, default: Option<&str>) -> Result<String, FlickError> {
        let mut input = dialoguer::Input::<String>::new().with_prompt(prompt);
        if let Some(d) = default {
            input = input.default(d.to_string());
        }
        input
            .interact_on(&self.term)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))
    }

    fn confirm(&self, prompt: &str, default: bool) -> Result<bool, FlickError> {
        dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(default)
            .interact_on(&self.term)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))
    }

    fn multi_select(
        &self,
        prompt: &str,
        items: &[String],
        defaults: &[bool],
    ) -> Result<Vec<usize>, FlickError> {
        dialoguer::MultiSelect::new()
            .with_prompt(prompt)
            .items(items)
            .defaults(defaults)
            .interact_on(&self.term)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))
    }

    fn message(&self, msg: &str) -> Result<(), FlickError> {
        self.term
            .write_line(msg)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))
    }
}

/// Test prompter with pre-programmed responses.
pub struct MockPrompter {
    passwords: std::sync::Mutex<std::collections::VecDeque<String>>,
    selects: std::sync::Mutex<std::collections::VecDeque<usize>>,
    inputs: std::sync::Mutex<std::collections::VecDeque<String>>,
    confirms: std::sync::Mutex<std::collections::VecDeque<bool>>,
    multi_selects: std::sync::Mutex<std::collections::VecDeque<Vec<usize>>>,
    messages: std::sync::Mutex<Vec<String>>,
}

impl Default for MockPrompter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPrompter {
    pub const fn new() -> Self {
        Self {
            passwords: std::sync::Mutex::new(std::collections::VecDeque::new()),
            selects: std::sync::Mutex::new(std::collections::VecDeque::new()),
            inputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
            confirms: std::sync::Mutex::new(std::collections::VecDeque::new()),
            multi_selects: std::sync::Mutex::new(std::collections::VecDeque::new()),
            messages: std::sync::Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn with_passwords(self, passwords: Vec<String>) -> Self {
        *self.passwords.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            passwords.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_selects(self, selects: Vec<usize>) -> Self {
        *self.selects.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            selects.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_inputs(self, inputs: Vec<String>) -> Self {
        *self.inputs.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            inputs.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_confirms(self, confirms: Vec<bool>) -> Self {
        *self.confirms.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            confirms.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_multi_selects(self, multi_selects: Vec<Vec<usize>>) -> Self {
        *self.multi_selects.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            multi_selects.into_iter().collect();
        self
    }

    /// Returns all messages sent via `message()`.
    pub fn collected_messages(&self) -> Vec<String> {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn pop_response<T>(
    queue: &std::sync::Mutex<std::collections::VecDeque<T>>,
    method: &str,
) -> Result<T, FlickError> {
    queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pop_front()
        .ok_or_else(|| {
            FlickError::Io(std::io::Error::other(format!(
                "MockPrompter: no more {method} responses"
            )))
        })
}

impl Prompter for MockPrompter {
    fn password(&self, _prompt: &str) -> Result<String, FlickError> {
        pop_response(&self.passwords, "password")
    }

    fn select(
        &self,
        _prompt: &str,
        _items: &[String],
        _default: usize,
    ) -> Result<usize, FlickError> {
        pop_response(&self.selects, "select")
    }

    fn input(&self, _prompt: &str, _default: Option<&str>) -> Result<String, FlickError> {
        pop_response(&self.inputs, "input")
    }

    fn confirm(&self, _prompt: &str, _default: bool) -> Result<bool, FlickError> {
        pop_response(&self.confirms, "confirm")
    }

    fn multi_select(
        &self,
        _prompt: &str,
        _items: &[String],
        _defaults: &[bool],
    ) -> Result<Vec<usize>, FlickError> {
        pop_response(&self.multi_selects, "multi_select")
    }

    fn message(&self, msg: &str) -> Result<(), FlickError> {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(msg.to_string());
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_responses_in_order() {
        let mock = MockPrompter::new()
            .with_passwords(vec!["pass1".into(), "pass2".into()])
            .with_selects(vec![0, 2])
            .with_inputs(vec!["input1".into()])
            .with_confirms(vec![true, false])
            .with_multi_selects(vec![vec![0, 1]]);

        assert_eq!(mock.password("p").expect("p1"), "pass1");
        assert_eq!(mock.password("p").expect("p2"), "pass2");
        assert_eq!(mock.select("s", &[], 0).expect("s1"), 0);
        assert_eq!(mock.select("s", &[], 0).expect("s2"), 2);
        assert_eq!(mock.input("i", None).expect("i1"), "input1");
        assert!(mock.confirm("c", false).expect("c1"));
        assert!(!mock.confirm("c", false).expect("c2"));
        assert_eq!(mock.multi_select("m", &[], &[]).expect("m1"), vec![0, 1]);
    }

    #[test]
    fn mock_errors_when_exhausted() {
        let mock = MockPrompter::new();
        assert!(mock.password("p").is_err());
        assert!(mock.select("s", &[], 0).is_err());
        assert!(mock.input("i", None).is_err());
        assert!(mock.confirm("c", false).is_err());
        assert!(mock.multi_select("m", &[], &[]).is_err());
    }

    #[test]
    fn mock_collects_messages() {
        let mock = MockPrompter::new();
        mock.message("hello").expect("msg1");
        mock.message("world").expect("msg2");
        assert_eq!(mock.collected_messages(), vec!["hello", "world"]);
    }
}
