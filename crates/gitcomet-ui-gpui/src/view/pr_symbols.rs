use crate::github::{ChecksSummary, ReviewDecision};
use crate::theme::{self, AppTheme, StatusColorSet};
use crate::view::icons::svg_icon;
use crate::view::tooltip::GitCometTooltipExt as _;
use gpui::{
    AnyElement, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Rgba, SharedString, Styled as _, div, px,
};

pub(super) struct Symbol {
    path: &'static str,
    pub(super) label: String,
    color: Rgba,
}

impl Symbol {
    /// The color this symbol's icon paints with, for a caller that wants to
    /// color accompanying text (e.g. a review verb) the same way.
    pub(super) fn color(&self) -> Rgba {
        self.color
    }

    pub(super) fn render(&self, id: String, theme: AppTheme, size: Pixels) -> AnyElement {
        div()
            .id(SharedString::from(id))
            .flex_none()
            .child(svg_icon(self.path, self.color, size))
            .gitcomet_tooltip(theme, SharedString::from(self.label.clone()))
            .into_any_element()
    }
}

/// The `@` badge marking a pull request that waits on your review.
pub(super) fn at_pill(theme: AppTheme, id: impl Into<ElementId>) -> AnyElement {
    div()
        .id(id.into())
        .flex_none()
        .rounded(px(4.0))
        .bg(theme.colors.accent.solid)
        .text_color(theme.colors.accent.on_solid)
        .px_1()
        .font_weight(FontWeight::SEMIBOLD)
        .child("@")
        .gitcomet_tooltip(theme, SharedString::from("Your review is requested"))
        .into_any_element()
}

/// The small `feat` / `fix` / … tag next to a pull request's stripped title:
/// mono, tinted by kind (feat info, fix warning, docs success, refactor purple);
/// anything else (`deps`) stays neutral.
pub(super) fn kind_tag(
    kind: &str,
    theme: AppTheme,
    text_size: impl Into<gpui::AbsoluteLength>,
) -> AnyElement {
    let (background, foreground) = match kind {
        "feat" => (
            theme.colors.status.info.background,
            theme.colors.status.info.foreground,
        ),
        "fix" => (
            theme.colors.status.warning.background,
            theme.colors.status.warning.foreground,
        ),
        "docs" => (
            theme.colors.status.success.background,
            theme.colors.status.success.foreground,
        ),
        "refactor" => {
            let purple = theme::historical_outline(theme.is_dark);
            (theme::with_alpha(purple, 0.16), purple)
        }
        _ => (
            theme.colors.surface.panel,
            theme.colors.foreground.secondary,
        ),
    };
    div()
        .flex_none()
        .rounded(px(3.0))
        .px_1()
        .bg(background)
        .font_family(crate::font_preferences::EDITOR_MONOSPACE_FONT_FAMILY)
        .text_size(text_size)
        .text_color(foreground)
        .child(kind.to_string())
        .into_any_element()
}

/// A small rounded pill for a review thread's status ("Open" / "Resolved" /
/// "Outdated"), colored from the matching status token.
pub(super) fn status_chip(label: &str, set: StatusColorSet, theme: AppTheme) -> AnyElement {
    status_pill(label, set, theme.ui_text(10.5), FontWeight::NORMAL, theme)
}

/// [`status_chip`] at a chosen size and weight (the "Reviewing" pill).
pub(super) fn status_pill(
    label: &str,
    set: StatusColorSet,
    text_size: impl Into<gpui::AbsoluteLength>,
    weight: FontWeight,
    theme: AppTheme,
) -> AnyElement {
    div()
        .flex_none()
        .rounded(px(theme.radii.pill))
        .border_1()
        .border_color(set.border)
        .bg(set.background)
        .text_color(set.foreground)
        .px_2()
        .text_size(text_size)
        .font_weight(weight)
        .child(label.to_string())
        .into_any_element()
}

/// One "Open N" / "Resolved N" / "Outdated N" filter chip in the Comments tab.
/// Not a click target: only one of these three groupings is ever an actual
/// filter (open-only vs. show-all, toggled by `V`), so the other two stay
/// informational counts rather than implying a filter this view doesn't have.
pub(super) fn filter_chip(label: String, selected: bool, theme: AppTheme) -> AnyElement {
    let mut chip = div()
        .flex_none()
        .rounded(px(theme.radii.pill))
        .border_1()
        .px_2()
        .text_size(theme.ui_text(11.0));
    chip = if selected {
        chip.border_color(theme.colors.accent.foreground)
            .bg(theme.colors.accent.subtle_background)
            .text_color(theme.colors.foreground.primary)
    } else {
        chip.border_color(theme.colors.stroke.default)
            .text_color(theme.colors.foreground.secondary)
    };
    chip.child(label).into_any_element()
}

/// A review thread's file group header: file icon, mono path, open count.
pub(super) fn file_header(
    path: &str,
    open_count: usize,
    theme: AppTheme,
    size: Pixels,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .pt_2()
        .text_size(theme.ui_text(12.0))
        .child(svg_icon(
            "icons/file.svg",
            theme.colors.foreground.secondary,
            size,
        ))
        .child(
            div()
                .font_family(crate::font_preferences::EDITOR_MONOSPACE_FONT_FAMILY)
                .text_color(theme.colors.foreground.primary)
                .child(path.to_string()),
        )
        .child(
            div()
                .text_color(theme.colors.foreground.secondary)
                .child(format!("{open_count} open")),
        )
        .into_any_element()
}

/// Speech-bubble icon + a count, for a review thread's reply count.
pub(super) fn bubble_count(count: usize, theme: AppTheme, size: Pixels) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_size(theme.ui_text(11.0))
        .text_color(theme.colors.foreground.secondary)
        .child(svg_icon(
            "icons/review_comment.svg",
            theme.colors.foreground.secondary,
            size,
        ))
        .child(count.to_string())
        .into_any_element()
}

/// "N line comments, on a.rs and b.rs" — a review entry's footer, once its
/// inline comments are matched to their review threads by REST review id
/// (`ConversationEntry::review_id` against `ReviewThread::pull_request_review_id`).
/// The text is one line that gives way with an ellipsis; the `]` Comments hint
/// after it never does.
pub(super) fn review_line_comments_footer(
    count: usize,
    paths: &[String],
    theme: AppTheme,
    ui_scale: crate::ui_scale::UiScale,
) -> AnyElement {
    let size = ui_scale.px(12.0);
    div()
        .flex()
        .items_center()
        .gap_2()
        .min_w(px(0.0))
        .text_size(theme.ui_text(12.0))
        .text_color(theme.colors.foreground.secondary)
        .child(svg_icon(
            "icons/review_comment.svg",
            theme.colors.foreground.secondary,
            size,
        ))
        .child(div().flex_1().min_w(px(0.0)).truncate().child(format!(
            "{count} line comment{}, on {}",
            if count == 1 { "" } else { "s" },
            footer_file_list(paths)
        )))
        // `]` opens the Comments tab, where these threads live.
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .ml_1()
                .child(crate::view::components::shortcut_keys("]", theme, ui_scale))
                .child("Comments"),
        )
        .into_any_element()
}

/// The file names (not paths) of a review's line comments: "a.rs",
/// "a.rs and b.rs", or "a.rs, b.rs and N more" past two.
fn footer_file_list(paths: &[String]) -> String {
    let names: Vec<&str> = paths
        .iter()
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .collect();
    match names.as_slice() {
        [] => String::new(),
        [only] => (*only).to_string(),
        [first, second] => format!("{first} and {second}"),
        [first, second, rest @ ..] => format!("{first}, {second} and {} more", rest.len()),
    }
}

/// "Description truncated · `o` opens it on GitHub": the closing line of a card
/// whose text was cut at its cap. `what` names the cut text.
pub(super) fn truncated_note(
    what: &str,
    theme: AppTheme,
    ui_scale: crate::ui_scale::UiScale,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .pt_2()
        .text_size(theme.ui_text(11.5))
        .text_color(theme.colors.foreground.secondary)
        .child(format!("{what} truncated ·"))
        .child(crate::view::components::shortcut_keys("o", theme, ui_scale))
        .child("opens it on GitHub")
        .into_any_element()
}

/// Maps a reviewer's status to the same icons `review` draws, when one
/// applies; `Dismissed` has none, so callers keep the `×` text for it.
pub(super) fn reviewer_status(
    status: crate::github::PrReviewerStatus,
    theme: AppTheme,
) -> Result<Symbol, &'static str> {
    use crate::github::PrReviewerStatus as Status;
    match status {
        Status::Approved => Ok(review(ReviewDecision::Approved, theme)),
        Status::ChangesRequested => Ok(review(ReviewDecision::ChangesRequested, theme)),
        Status::Commented => Ok(review(ReviewDecision::Commented, theme)),
        Status::Requested => Ok(review(ReviewDecision::ReviewRequired, theme)),
        Status::Dismissed => Err("×"),
    }
}

pub(super) fn state(state: &str, draft: bool, theme: AppTheme) -> Symbol {
    let (path, label, color) = match state {
        "MERGED" => (
            "icons/git_merge.svg",
            "Merged",
            theme::historical_outline(theme.is_dark),
        ),
        "CLOSED" => (
            "icons/pull_request_closed.svg",
            "Closed",
            theme.colors.status.danger.foreground,
        ),
        _ if draft => (
            "icons/pull_request_draft.svg",
            "Draft",
            theme.colors.foreground.secondary,
        ),
        _ => (
            "icons/pull_request.svg",
            "Open",
            theme.colors.status.success.foreground,
        ),
    };
    Symbol {
        path,
        label: label.into(),
        color,
    }
}

pub(super) fn review(review: ReviewDecision, theme: AppTheme) -> Symbol {
    let (path, color) = match review {
        ReviewDecision::ReviewRequired => (
            "icons/review_required.svg",
            theme.colors.status.warning.foreground,
        ),
        ReviewDecision::Approved => (
            "icons/review_approved.svg",
            theme.colors.status.success.foreground,
        ),
        ReviewDecision::ChangesRequested => (
            "icons/review_changes_requested.svg",
            theme.colors.status.danger.foreground,
        ),
        ReviewDecision::Commented => (
            "icons/review_comment.svg",
            theme.colors.foreground.secondary,
        ),
    };
    Symbol {
        path,
        label: review.label().into(),
        color,
    }
}

pub(super) fn checks(checks: ChecksSummary, theme: AppTheme) -> Option<Symbol> {
    if checks.total() == 0 {
        return None;
    }
    let (path, label, color) = if checks.failing > 0 {
        (
            "icons/generic_close.svg",
            format!("{} failing checks", checks.failing),
            theme.colors.status.danger.foreground,
        )
    } else if checks.pending > 0 {
        (
            "icons/spinner.svg",
            format!("{} running checks", checks.pending),
            theme.colors.status.warning.foreground,
        )
    } else {
        (
            "icons/check.svg",
            format!("{} passing checks", checks.passing),
            theme.colors.status.success.foreground,
        )
    };
    Some(Symbol { path, label, color })
}

/// Icon + "N of TOTAL checks passed/failing/pending on SHA", for the
/// conversation footer. `None` when the pull request has no checks at all.
pub(super) fn checks_line(
    checks: ChecksSummary,
    sha: &str,
    theme: AppTheme,
    size: Pixels,
) -> Option<AnyElement> {
    let total = checks.total();
    if total == 0 {
        return None;
    }
    let short_sha = sha.get(..7).unwrap_or(sha);
    let (path, color, text) = if checks.failing > 0 {
        (
            "icons/generic_close.svg",
            theme.colors.status.danger.foreground,
            format!(
                "{} of {total} checks failing on {short_sha}",
                checks.failing
            ),
        )
    } else if checks.pending > 0 {
        (
            "icons/spinner.svg",
            theme.colors.status.warning.foreground,
            format!(
                "{} of {total} checks pending on {short_sha}",
                checks.pending
            ),
        )
    } else {
        (
            "icons/check.svg",
            theme.colors.status.success.foreground,
            format!("{} of {total} checks passed on {short_sha}", checks.passing),
        )
    };
    Some(
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_color(theme.colors.foreground.secondary)
            .child(svg_icon(path, color, size))
            .child(text)
            .into_any_element(),
    )
}

pub(super) fn person(theme: AppTheme) -> Symbol {
    Symbol {
        path: "icons/person.svg",
        label: "Your pull request".into(),
        color: theme.colors.foreground.secondary,
    }
}

pub(super) fn pending(count: usize, theme: AppTheme) -> Symbol {
    Symbol {
        path: "icons/pencil.svg",
        label: format!("{count} pending review comments"),
        color: theme.colors.status.warning.foreground,
    }
}

pub(super) fn title(title: &str) -> (Option<&'static str>, &str) {
    let Some(colon) = title.find(':') else {
        return (None, title);
    };
    let prefix = &title[..colon];
    let prefix = prefix.strip_suffix('!').unwrap_or(prefix);
    let name = prefix.split_once('(').map_or(prefix, |(name, _)| name);
    if let Some((_, scope)) = prefix.split_once('(')
        && (scope.len() < 2 || !scope.ends_with(')') || scope[..scope.len() - 1].trim().is_empty())
    {
        return (None, title);
    }
    let kind = if name.eq_ignore_ascii_case("feat") {
        "feat"
    } else if name.eq_ignore_ascii_case("fix") {
        "fix"
    } else if name.eq_ignore_ascii_case("refactor") {
        "refactor"
    } else if name.eq_ignore_ascii_case("docs") {
        "docs"
    } else if ["deps", "chore", "ci", "build"]
        .iter()
        .any(|p| name.eq_ignore_ascii_case(p))
    {
        "deps"
    } else {
        return (None, title);
    };
    let rest = title[colon + 1..].trim_start();
    if rest.is_empty() {
        (None, title)
    } else {
        (Some(kind), rest)
    }
}

#[cfg(test)]
mod tests {
    use super::{footer_file_list, title};

    #[test]
    fn footer_lists_file_names_not_paths_and_at_most_two() {
        let paths = |list: &[&str]| list.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert_eq!(footer_file_list(&[]), "");
        assert_eq!(footer_file_list(&paths(&["a.rs"])), "a.rs");
        assert_eq!(
            footer_file_list(&paths(&["Assets/Game/a.cs", "Assets/Retention/b.cs"])),
            "a.cs and b.cs"
        );
        assert_eq!(
            footer_file_list(&paths(&["x/a.rs", "y/b.rs", "z/c.rs"])),
            "a.rs, b.rs and 1 more"
        );
        assert_eq!(
            footer_file_list(&paths(&["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"])),
            "a.rs, b.rs and 3 more"
        );
    }

    #[test]
    fn title_kind_only_strips_recognized_prefixes() {
        assert_eq!(
            title("feat(ui): Improve list"),
            (Some("feat"), "Improve list")
        );
        assert_eq!(
            title("chore: update crates"),
            (Some("deps"), "update crates")
        );
        assert_eq!(title("fix: Crash"), (Some("fix"), "Crash"));
        assert_eq!(title("feature: Keep this"), (None, "feature: Keep this"));
        assert_eq!(title("docs(): Keep this"), (None, "docs(): Keep this"));
        assert_eq!(title("feat:"), (None, "feat:"));
    }

    #[test]
    fn title_strips_the_breaking_change_marker() {
        assert_eq!(title("feat!: drop X"), (Some("feat"), "drop X"));
        assert_eq!(title("feat(ui)!: x"), (Some("feat"), "x"));
        assert_eq!(title("fixture tests"), (None, "fixture tests"));
        assert_eq!(title("Fix: typo"), (Some("fix"), "typo"));
    }
}
