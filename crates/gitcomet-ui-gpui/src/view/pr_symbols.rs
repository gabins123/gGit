use crate::github::{ChecksSummary, ReviewDecision};
use crate::theme::{self, AppTheme};
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

/// The small `feat` / `fix` / … tag next to a pull request's stripped title.
pub(super) fn kind_tag(
    kind: &str,
    theme: AppTheme,
    text_size: impl Into<gpui::AbsoluteLength>,
) -> AnyElement {
    div()
        .flex_none()
        .rounded(px(3.0))
        .px_1()
        .bg(theme.colors.surface.panel)
        .text_size(text_size)
        .text_color(theme.colors.foreground.secondary)
        .child(kind.to_string())
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
    use super::title;

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
