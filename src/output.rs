use colored::*;

/// Replace control characters so text from outside the trust boundary cannot
/// move the cursor, clear the screen, or conceal what was already printed.
///
/// RPC error strings carry a server-supplied JSON-RPC `message` through to
/// `Display` verbatim, and JSON escapes decode to real ESC bytes. Several lines
/// here are deliberately unforgeable by remote data - the create_key/PDA binding,
/// the byte-exact instruction classification, the rekey warning - and cursor
/// repainting would let an endpoint overwrite exactly those, at the last inch
/// before a signing decision. Newlines and tabs are kept; everything else in the
/// control range becomes a visible replacement character.
pub fn scrub(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' || c == '\t' {
                c
            } else if c.is_control() {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

/// Centralized output formatting for consistent UI throughout the application
pub struct Output;

impl Output {
    /// Success message with green checkmark
    pub fn success(msg: &str) {
        println!("{} {}", "✅".bright_green(), scrub(msg));
    }

    /// Information message with blue info icon
    pub fn info(msg: &str) {
        println!("{} {}", "ℹ️".bright_blue(), scrub(msg));
    }

    /// Warning message with yellow warning icon
    pub fn warning(msg: &str) {
        println!("{} {}", "⚠️".bright_yellow(), scrub(msg));
    }

    /// Error message with red X icon
    pub fn error(msg: &str) {
        println!("{} {}", "❌".bright_red(), scrub(msg));
    }

    /// Header with yellow bold text
    pub fn header(msg: &str) {
        println!("{}", scrub(msg).bright_yellow().bold());
    }

    /// Field display with cyan key and white value
    pub fn field(key: &str, value: &str) {
        println!("  {}: {}", key.cyan(), scrub(value).bright_white());
    }

    /// Numbered field display (for lists)
    pub fn numbered_field(index: usize, key: &str, value: &str) {
        println!(
            "    {}: {}",
            format!("{} {}", index, key).cyan(),
            scrub(value).bright_white()
        );
    }

    /// Hint message with blue lightbulb
    pub fn hint(msg: &str) {
        println!("{} {}", "💡 Hint:".bright_blue(), scrub(msg));
    }

    /// Separator line for sections
    pub fn separator() {
        println!();
    }

    /// Configuration display with special formatting
    pub fn config_item(key: &str, value: &str) {
        println!(
            "  {}: {}",
            key.cyan(),
            if value.is_empty() || value == "None" {
                "Not configured".bright_yellow()
            } else {
                scrub(value).bright_white()
            }
        );
    }

    /// Address display with consistent formatting
    pub fn address(label: &str, addr: &str) {
        println!("  {}: {}", label.cyan(), scrub(addr).bright_white());
    }
}

#[cfg(test)]
mod tests {
    use super::scrub;

    /// The sequences that matter are the ones that move or erase, since those
    /// let remote text overwrite locally derived lines a signer is relying on.
    #[test]
    fn scrub_neutralises_cursor_and_erase_sequences() {
        // Screen clear + cursor home, as an RPC error message could carry.
        let repaint = "\u{1b}[2J\u{1b}[H fabricated activation";
        let cleaned = scrub(repaint);
        assert!(!cleaned.contains('\u{1b}'), "ESC survived: {cleaned:?}");
        assert!(cleaned.ends_with(" fabricated activation"));

        // Cursor-up and carriage return would overwrite the previous lines.
        for sequence in ["\u{1b}[3A", "\r", "\u{8}", "\u{1b}[1K"] {
            let cleaned = scrub(sequence);
            assert!(
                !cleaned.contains('\u{1b}')
                    && !cleaned.contains('\r')
                    && !cleaned.contains('\u{8}'),
                "{sequence:?} survived as {cleaned:?}"
            );
        }

        // Newline and tab are ordinary layout and are kept, so multi-line
        // messages (simulation logs) still read correctly.
        assert_eq!(scrub("a\nb\tc"), "a\nb\tc");
        // Ordinary text is untouched.
        assert_eq!(
            scrub("AccountNotFound: pubkey=abc"),
            "AccountNotFound: pubkey=abc"
        );
    }
}
