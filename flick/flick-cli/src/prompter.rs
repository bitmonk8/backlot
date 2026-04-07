use std::io::Write as _;

use flick::error::FlickError;

/// Abstraction over interactive prompts, mockable in tests.
pub trait Prompter {
    /// Display a password prompt (hidden input). Returns the entered string.
    fn password(&self, prompt: &str) -> Result<String, FlickError>;

    /// Display a selection list. Returns the index of the selected item.
    fn select(&self, prompt: &str, items: &[String], default: usize) -> Result<usize, FlickError>;

    /// Display a text input with an optional default.
    /// Returns the entered string (or default if empty).
    fn input(&self, prompt: &str, default: Option<&str>) -> Result<String, FlickError>;

    /// Print a message to the user (stderr).
    fn message(&self, msg: &str) -> Result<(), FlickError>;
}

/// Production prompter using `rpassword` for hidden input and
/// plain stdin/stderr for text input and selection. All output targets stderr.
pub struct TerminalPrompter;

impl Default for TerminalPrompter {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalPrompter {
    pub fn new() -> Self {
        Self
    }
}

impl Prompter for TerminalPrompter {
    fn password(&self, prompt: &str) -> Result<String, FlickError> {
        rpassword::prompt_password(format!("{prompt}: ")).map_err(FlickError::Io)
    }

    fn select(&self, prompt: &str, items: &[String], default: usize) -> Result<usize, FlickError> {
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "{prompt}").map_err(FlickError::Io)?;
        for (i, item) in items.iter().enumerate() {
            let marker = if i == default { ">" } else { " " };
            writeln!(stderr, "  {marker} [{i}] {item}").map_err(FlickError::Io)?;
        }
        write!(stderr, "Selection [default: {default}]: ").map_err(FlickError::Io)?;
        stderr.flush().map_err(FlickError::Io)?;
        drop(stderr);

        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(FlickError::Io)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(default);
        }
        let idx = trimmed.parse::<usize>().map_err(|e| {
            FlickError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
        })?;
        if idx >= items.len() {
            return Err(FlickError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("selection {idx} out of range (0..{})", items.len()),
            )));
        }
        Ok(idx)
    }

    fn input(&self, prompt: &str, default: Option<&str>) -> Result<String, FlickError> {
        let mut stderr = std::io::stderr().lock();
        match default {
            Some(d) => write!(stderr, "{prompt} [{d}]: ").map_err(FlickError::Io)?,
            None => write!(stderr, "{prompt}: ").map_err(FlickError::Io)?,
        }
        stderr.flush().map_err(FlickError::Io)?;
        drop(stderr);

        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(FlickError::Io)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if let Some(d) = default {
                return Ok(d.to_string());
            }
        }
        Ok(trimmed.to_string())
    }

    fn message(&self, msg: &str) -> Result<(), FlickError> {
        writeln!(std::io::stderr(), "{msg}").map_err(FlickError::Io)
    }
}

#[cfg(test)]
/// Test prompter with pre-programmed responses.
pub struct MockPrompter {
    passwords: std::sync::Mutex<std::collections::VecDeque<String>>,
    selects: std::sync::Mutex<std::collections::VecDeque<usize>>,
    inputs: std::sync::Mutex<std::collections::VecDeque<String>>,
    messages: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl Default for MockPrompter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl MockPrompter {
    pub const fn new() -> Self {
        Self {
            passwords: std::sync::Mutex::new(std::collections::VecDeque::new()),
            selects: std::sync::Mutex::new(std::collections::VecDeque::new()),
            inputs: std::sync::Mutex::new(std::collections::VecDeque::new()),
            messages: std::sync::Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn with_passwords(self, passwords: Vec<String>) -> Self {
        *self
            .passwords
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = passwords.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_selects(self, selects: Vec<usize>) -> Self {
        *self
            .selects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = selects.into_iter().collect();
        self
    }

    #[must_use]
    pub fn with_inputs(self, inputs: Vec<String>) -> Self {
        *self
            .inputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = inputs.into_iter().collect();
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

#[cfg(test)]
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

#[cfg(test)]
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
            .with_inputs(vec!["input1".into()]);

        assert_eq!(mock.password("p").expect("p1"), "pass1");
        assert_eq!(mock.password("p").expect("p2"), "pass2");
        assert_eq!(mock.select("s", &[], 0).expect("s1"), 0);
        assert_eq!(mock.select("s", &[], 0).expect("s2"), 2);
        assert_eq!(mock.input("i", None).expect("i1"), "input1");
    }

    #[test]
    fn mock_errors_when_exhausted() {
        let mock = MockPrompter::new();
        assert!(mock.password("p").is_err());
        assert!(mock.select("s", &[], 0).is_err());
        assert!(mock.input("i", None).is_err());
    }

    #[test]
    fn mock_collects_messages() {
        let mock = MockPrompter::new();
        mock.message("hello").expect("msg1");
        mock.message("world").expect("msg2");
        assert_eq!(mock.collected_messages(), vec!["hello", "world"]);
    }
}
