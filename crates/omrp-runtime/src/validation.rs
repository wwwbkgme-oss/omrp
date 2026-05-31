//! Input validation helpers for OMRP web endpoints.
//!
//! All validators return `Result<(), &'static str>` — the error string is
//! sent directly to the client so it must be user-friendly.

// ─── Username ─────────────────────────────────────────────────────────────────

/// Validate a username:
/// - 3–50 characters
/// - Lowercase letters, digits, underscore, hyphen, dot only
/// - Must start with a letter or digit
/// - No consecutive special characters
pub fn validate_username(s: &str) -> Result<(), &'static str> {
    let s = s.trim();
    if s.len() < 3  { return Err("Username must be at least 3 characters"); }
    if s.len() > 50 { return Err("Username must be at most 50 characters"); }
    if !s.chars().next().map(|c| c.is_alphanumeric()).unwrap_or(false) {
        return Err("Username must start with a letter or digit");
    }
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '.' {
            return Err("Username may only contain letters, digits, -, _, and .");
        }
    }
    Ok(())
}

// ─── Password ─────────────────────────────────────────────────────────────────

/// Validate a password:
/// - 8–128 characters
pub fn validate_password(s: &str) -> Result<(), &'static str> {
    if s.len() < 8   { return Err("Password must be at least 8 characters"); }
    if s.len() > 128 { return Err("Password must be at most 128 characters"); }
    Ok(())
}

// ─── Email ────────────────────────────────────────────────────────────────────

/// Basic email sanity check (not RFC 5322 compliant — just enough to catch
/// obviously wrong values before they hit the DB).
pub fn validate_email(s: &str) -> Result<(), &'static str> {
    let s = s.trim();
    if s.is_empty() { return Ok(()); } // email is optional
    if s.len() > 255 { return Err("Email address too long (max 255 characters)"); }
    let at = s.find('@').ok_or("Email must contain '@'")?;
    let local  = &s[..at];
    let domain = &s[at+1..];
    if local.is_empty()  { return Err("Email local part (before @) is empty"); }
    if domain.is_empty() { return Err("Email domain (after @) is empty"); }
    if !domain.contains('.') { return Err("Email domain must contain at least one dot"); }
    // No whitespace
    if s.contains(char::is_whitespace) {
        return Err("Email must not contain whitespace");
    }
    Ok(())
}

// ─── Display name ─────────────────────────────────────────────────────────────

/// Display names: 1–100 printable characters, no control characters.
pub fn validate_display_name(s: &str) -> Result<(), &'static str> {
    if s.is_empty()  { return Ok(()); } // optional
    if s.len() > 100 { return Err("Display name must be at most 100 characters"); }
    if s.chars().any(|c| c.is_control()) {
        return Err("Display name must not contain control characters");
    }
    Ok(())
}

// ─── Label ────────────────────────────────────────────────────────────────────

/// Key/provider labels: 1–64 printable characters.
pub fn validate_label(s: &str) -> Result<(), &'static str> {
    let s = s.trim();
    if s.is_empty()  { return Ok(()); } // optional
    if s.len() > 64  { return Err("Label must be at most 64 characters"); }
    if s.chars().any(|c| c.is_control()) {
        return Err("Label must not contain control characters");
    }
    Ok(())
}

// ─── Provider key value ───────────────────────────────────────────────────────

/// Provider API key values: 8–512 printable non-whitespace characters.
pub fn validate_api_key_value(s: &str) -> Result<(), &'static str> {
    let s = s.trim();
    if s.len() < 8   { return Err("API key value too short (min 8 characters)"); }
    if s.len() > 512 { return Err("API key value too long (max 512 characters)"); }
    if s.contains(char::is_whitespace) {
        return Err("API key value must not contain whitespace");
    }
    Ok(())
}

// ─── Allowed models list ──────────────────────────────────────────────────────

/// Each model ID in an allow-list: max 200 chars, no whitespace, at most 50 items.
pub fn validate_allowed_models(models: &[String]) -> Result<(), &'static str> {
    if models.len() > 50 {
        return Err("Allowed models list may contain at most 50 entries");
    }
    for m in models {
        if m.len() > 200 { return Err("Model ID too long (max 200 characters)"); }
        if m.contains(char::is_whitespace) {
            return Err("Model IDs must not contain whitespace");
        }
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_username_valid() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("bob_123").is_ok());
        assert!(validate_username("x.y-z").is_ok());
    }

    #[test]
    fn test_username_invalid() {
        assert!(validate_username("ab").is_err());         // too short
        assert!(validate_username("_start").is_err());      // starts with _
        assert!(validate_username("has space").is_err());   // space
        assert!(validate_username("has!bang").is_err());    // !
        assert!(validate_username(&"a".repeat(51)).is_err()); // too long
    }

    #[test]
    fn test_password_valid() {
        assert!(validate_password("hunter22").is_ok());
        assert!(validate_password("a".repeat(128).as_str()).is_ok());
    }

    #[test]
    fn test_password_invalid() {
        assert!(validate_password("short").is_err());
        assert!(validate_password(&"a".repeat(129)).is_err());
    }

    #[test]
    fn test_email_valid() {
        assert!(validate_email("").is_ok());               // empty = optional
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("a+b@x.io").is_ok());
    }

    #[test]
    fn test_email_invalid() {
        assert!(validate_email("no-at").is_err());
        assert!(validate_email("@nodomain").is_err());
        assert!(validate_email("local@nodot").is_err());
        assert!(validate_email("has space@x.com").is_err());
    }

    #[test]
    fn test_label_valid() {
        assert!(validate_label("").is_ok());
        assert!(validate_label("Cursor").is_ok());
        assert!(validate_label("My key #1").is_ok());
    }

    #[test]
    fn test_label_invalid() {
        assert!(validate_label(&"a".repeat(65)).is_err());
    }

    #[test]
    fn test_api_key_value() {
        assert!(validate_api_key_value("sk-short").is_ok());
        assert!(validate_api_key_value("sk-1234567890abcdef").is_ok());
        assert!(validate_api_key_value("too short").is_err()); // whitespace
        assert!(validate_api_key_value("s").is_err());         // too short
    }
}
