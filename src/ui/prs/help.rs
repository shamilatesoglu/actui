//! The second page of the help: every key on the PR screens.

use super::*;
use ratatui::text::Text;

/// The left and right columns of the pull requests page.
pub(in crate::ui) fn pr_help() -> (Text<'static>, Text<'static>) {
    let left = Text::from(vec![
        hl("PR list  (P opens it)"),
        help_row("j / k, g / G", "move · top / bottom"),
        help_row("⏎", "open the PR"),
        help_row("o  ·  r", "open on GitHub · reload"),
        help_row("Esc / q / P", "back to the runs"),
        help_row("✎ after a PR", "it has unsent review comments"),
        Line::raw(""),
        hl("An open PR"),
        help_row("Tab / S-Tab", "Overview · Files · Conversation"),
        help_row("j / k", "scroll (Overview, Conversation)"),
        help_row("PgUp / PgDn", "page the Conversation · g / G ends"),
        help_row("c", "comment on the PR"),
        help_row("D", "mark ready for review / convert to draft"),
        help_row("m", "merge, or add to / remove from the queue"),
        help_row("  ↳ merge box", "j/k method · d delete branch · ⏎ merge"),
        help_row("u", "update the branch when it is behind"),
        help_row("x", "re-run the failed Actions checks"),
        help_row("o  ·  r", "open on GitHub · refresh"),
        help_row("Esc / Bksp", "back to the list"),
        Line::raw(""),
        hl("Overview"),
        help_row("J / K", "pick a check"),
        help_row("⏎", "an Actions check: its run · else its page"),
    ]);

    let right = Text::from(vec![
        hl("Files"),
        help_row("j / k, g / G", "move · top / bottom"),
        help_row("⏎ / l", "open the file's diff"),
        help_row("h / Esc", "back to the file list"),
        help_row("  ↳ in a diff", "j/k move · PgUp/PgDn page"),
        help_row("/  ·  n / N", "search · next / previous match"),
        help_row("v", "mark the start of a range · again clears"),
        help_row("c", "comment on the line or the range"),
        help_row("⏎", "read the threads on a line"),
        help_row("  ↳ in a thread", "r reply · x resolve · n/N thread"),
        Line::raw(""),
        hl("Reviewing  (any tab)"),
        help_row("p", "the pending review's comments"),
        help_row("  ↳ pending", "j/k move · e edit · d drop · R submit"),
        help_row("R", "submit the review: pick a verdict"),
        help_row("  ↳ verdict", "j/k pick · ⏎ write the message"),
        Line::raw(""),
        hl("Writing a comment"),
        help_row("Ctrl+S", "add to the review · send · submit"),
        help_row("Ctrl+G", "post a line comment alone, now"),
        help_row("Esc", "cancel (asks if you typed anything)"),
        Line::raw(""),
        hl("Quitting"),
        help_row("q / Ctrl-C", "asks first when comments are unsent"),
    ]);
    (left, right)
}
