//! Boxes drawn over the PR screens.

use super::*;
use crate::app::Compose;

/// The comment box, over whatever the PR screen shows.
pub(in crate::ui) fn draw_compose(f: &mut Frame, app: &mut App) {
    if !app.shows_prs() {
        return;
    }
    let Some(view) = app.prs.open.as_mut() else { return };
    let Some(Compose { target, editor }) = view.compose.as_mut() else { return };
    let title = format!("{} {}", target.title(), view.pr.label());
    let inner = popup(f, centered(70, 50, f.area()), &title, accent());
    let [text_area, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    editor.set_style(Style::default().fg(text()).bg(popup_bg()));
    editor.set_cursor_line_style(Style::default());
    f.render_widget(&*editor, text_area);
    f.render_widget(
        Paragraph::new(Span::styled("Ctrl+S send · Esc cancel", Style::default().fg(dim()))),
        hint_area,
    );
}
