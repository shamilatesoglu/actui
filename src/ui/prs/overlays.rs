//! Boxes drawn over the PR screens.

use super::*;
use crate::app::Compose;
use crate::pulls::model::blocked_reason;

/// The merge box, over whatever the PR screen shows.
pub(in crate::ui) fn draw_merge_dialog(f: &mut Frame, app: &mut App) {
    if !app.shows_prs() {
        return;
    }
    let Some(view) = app.prs.open.as_ref() else { return };
    let (Some(dialog), Loadable::Ready(detail)) = (&view.merge_dialog, &view.detail) else { return };
    let inner = popup(f, centered(60, 50, f.area()), &format!("Merge {}", view.pr.label()), accent());
    let [body, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let mut lines = Vec::new();
    if let Some(reason) = blocked_reason(detail) {
        lines.push(Line::from(Span::styled(
            format!("Blocked: {reason}. Merging works only if you can bypass branch protection."),
            Style::default().fg(soft_red()),
        )));
        lines.push(Line::default());
    }
    for (i, method) in dialog.methods.iter().enumerate() {
        let picked = i == dialog.sel;
        let style = if picked { Style::default().fg(accent()).add_modifier(Modifier::BOLD) } else { Style::default() };
        lines.push(Line::from(Span::styled(format!("{} {}", if picked { "›" } else { " " }, method.title()), style)));
    }
    if let Some(on) = dialog.delete_branch {
        lines.push(Line::default());
        lines.push(Line::from(format!("[{}] Delete branch {} after merging", if on { "x" } else { " " }, detail.head.branch)));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    let delete = if dialog.delete_branch.is_some() { " · d delete branch" } else { "" };
    f.render_widget(
        Paragraph::new(Span::styled(format!("j/k method{delete} · ⏎ merge · Esc cancel"), Style::default().fg(dim()))),
        hint_area,
    );
}

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
