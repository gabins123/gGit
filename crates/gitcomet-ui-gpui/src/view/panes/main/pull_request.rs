use super::*;
use crate::github::{
    ConversationEntry, PrReviewerStatus, PullRequestCommit, PullRequestDetail, ReviewThread,
};
use crate::kit::interaction::{self as controls, ControlInteractionExt as _};
use crate::theme::with_alpha;
use crate::view::RemoteMarkdownImagePolicy;
use crate::view::markdown_preview::{
    MarkdownInlineSpan, MarkdownInlineStyle, MarkdownPreviewDocument, MarkdownPreviewRowKind,
};
use crate::view::pull_requests::{
    MAX_PR_VISIBLE_THREADS, PrContentTab, PrLoad, visible_pr_thread_indexes,
};
use gpui::Div;
use rustc_hash::FxHashMap;

/// Readable-column cap for the Conversation and Comments bodies, matching the
/// design canvas.
const PR_CONTENT_MAX_WIDTH_PX: f32 = 780.0;
/// Avatar size for a top-level conversation entry / thread comment.
const MAIN_AVATAR_DIAMETER_PX: f32 = 28.0;
const MAIN_AVATAR_FONT_PX: f32 = 10.0;
/// Avatar size for a reviewer row or a review-thread reply.
const SMALL_AVATAR_DIAMETER_PX: f32 = 20.0;
const SMALL_AVATAR_FONT_PX: f32 = 9.0;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(in crate::view) enum PrMarkdownKey {
    Body,
    Entry(String),
    Thread(u64),
    /// A reply within a thread, by its position after the thread's first
    /// comment (which uses [`PrMarkdownKey::Thread`]).
    ThreadReply(u64, usize),
}

#[derive(Default)]
pub(in crate::view) struct PrMarkdownCache {
    number: Option<u64>,
    documents: FxHashMap<PrMarkdownKey, (String, Option<Arc<MarkdownPreviewDocument>>)>,
}

impl PrMarkdownCache {
    fn document(
        &mut self,
        number: u64,
        key: PrMarkdownKey,
        source: &str,
    ) -> Option<Arc<MarkdownPreviewDocument>> {
        if self.number != Some(number) {
            self.documents.clear();
            self.number = Some(number);
        }
        if self
            .documents
            .get(&key)
            .is_some_and(|(cached, _)| cached == source)
        {
            return self
                .documents
                .get(&key)
                .and_then(|(_, document)| document.clone());
        }
        if self.documents.len() >= 512 {
            self.documents.clear();
        }
        let document = pr_markdown_document(source).map(Arc::new);
        self.documents
            .insert(key, (source.to_owned(), document.clone()));
        document
    }
}

/// Groups a PR's commits by the conversation gap their commit time falls in
/// (GitHub gives commit times rather than push times, so this is the closest
/// approximation of "pushed with this comment/review"). One group per gap
/// before each conversation entry, plus a trailing group for commits pushed
/// after the last entry.
fn pr_pushed_commit_groups<'a>(
    commits: &'a [PullRequestCommit],
    entries: &[ConversationEntry],
) -> Vec<Vec<&'a PullRequestCommit>> {
    let mut sorted: Vec<&PullRequestCommit> = commits.iter().collect();
    sorted.sort_by(|a, b| a.committed_at.cmp(&b.committed_at));
    let mut next = 0;
    let mut groups = Vec::with_capacity(entries.len() + 1);
    for entry in entries {
        let start = next;
        while next < sorted.len() && sorted[next].committed_at.as_str() <= entry.at.as_str() {
            next += 1;
        }
        groups.push(sorted[start..next].to_vec());
    }
    groups.push(sorted[next..].to_vec());
    groups
}

/// A GitHub ISO 8601 timestamp as a coarse relative duration ("5 hours ago"),
/// falling back to the bare date when it doesn't parse.
fn pr_relative_time(at: &str, now: std::time::SystemTime) -> String {
    at.parse::<jiff::Timestamp>()
        .ok()
        .map(|ts| crate::view::date_time::format_relative_time(ts.as_second(), now))
        .unwrap_or_else(|| at.get(..10).unwrap_or(at).to_owned())
}

/// One line of a diff hunk's tail, for the selected review thread's context
/// strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffHunkLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiffHunkLine {
    kind: DiffHunkLineKind,
    /// The new-file line for a context or added line, the old-file line for a
    /// removed one — `None` only if the caller never gets a row (the header
    /// always resolves both, once it parses at all).
    line_number: Option<u32>,
    text: String,
}

/// A hunk longer than this many content lines never finishes its walk: only
/// the tail's line numbers matter, and GitHub's own hunks are always small,
/// so this only guards a pathological or hostile `diff_hunk`.
const MAX_HUNK_SCAN_LINES: usize = 2_000;
/// Longest a single rendered hunk line gets, before truncation.
const MAX_HUNK_LINE_CHARS: usize = 400;

/// The last `max_lines` content lines of a unified-diff hunk (GitHub always
/// ends one at the commented line), with each line's number derived by
/// walking the hunk from its header. `diff_hunk` is untrusted plain text:
/// never treated as markup, never interpreted beyond this line-by-line walk.
fn diff_hunk_tail(hunk: &str, max_lines: usize) -> Vec<DiffHunkLine> {
    let mut lines = hunk.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let Some(parsed) = crate::view::diff_utils::parse_unified_hunk_header_for_display(header)
    else {
        return Vec::new();
    };
    let mut old_line = parsed.old_start_line;
    let mut new_line = parsed.new_start_line;
    let mut rows = Vec::new();
    for line in lines.take(MAX_HUNK_SCAN_LINES) {
        let (kind, content) = match line.as_bytes().first() {
            Some(b'+') => (DiffHunkLineKind::Added, &line[1..]),
            Some(b'-') => (DiffHunkLineKind::Removed, &line[1..]),
            // "\ No newline at end of file".
            Some(b'\\') => continue,
            _ => (DiffHunkLineKind::Context, line.get(1..).unwrap_or("")),
        };
        let line_number = match kind {
            DiffHunkLineKind::Added => {
                let number = new_line;
                new_line += 1;
                number
            }
            DiffHunkLineKind::Removed => {
                let number = old_line;
                old_line += 1;
                number
            }
            DiffHunkLineKind::Context => {
                let number = new_line;
                old_line += 1;
                new_line += 1;
                number
            }
        };
        rows.push(DiffHunkLine {
            kind,
            line_number: Some(line_number),
            text: content.chars().take(MAX_HUNK_LINE_CHARS).collect(),
        });
    }
    let start = rows.len().saturating_sub(max_lines);
    rows.split_off(start)
}

/// Elevated card shared by every Conversation entry and the selected review
/// thread: 1px stroke, `radii.control` corners. `current` adds the accent
/// border and soft ring the design canvas uses for the keyboard-selected
/// entry (`.card.cur`).
fn pr_card(theme: AppTheme, current: bool) -> Div {
    let card = div()
        .flex()
        .flex_col()
        .min_w(px(0.0))
        .bg(theme.colors.surface.raised)
        .border_1()
        .border_color(theme.colors.stroke.subtle)
        .rounded(px(theme.radii.control))
        .overflow_hidden();
    if current {
        card.border_color(theme.colors.accent.foreground)
            .shadow(vec![gpui::BoxShadow {
                color: with_alpha(theme.colors.accent.foreground, 0.18).into(),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(0.0),
                spread_radius: px(2.0),
                inset: false,
            }])
    } else {
        card
    }
}

impl MainPaneView {
    pub(super) fn pull_request_view(&mut self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let theme = self.theme;
        let Some(root) = self.root_view.upgrade() else {
            return div().into_any_element();
        };
        let (number, detail, threads, tab, selected_entry, selected_thread, show_hidden) = {
            let root = root.read(cx);
            let Some(prs) = root.active_pull_requests() else {
                return div().into_any_element();
            };
            (
                prs.selected.unwrap_or_default(),
                prs.detail.clone(),
                prs.threads.clone(),
                prs.content_tab,
                prs.selected_entry,
                prs.selected_thread,
                prs.show_hidden_threads,
            )
        };
        let detail = match detail {
            PrLoad::Idle | PrLoad::Loading => {
                return components::empty_state_message(theme, format!("Loading #{number}…"))
                    .into_any_element();
            }
            PrLoad::Failed(err) => {
                return components::empty_state(
                    theme,
                    format!("Couldn't load #{number}"),
                    err.to_string(),
                )
                .into_any_element();
            }
            PrLoad::Ready(detail) => detail,
        };

        let selection = match tab {
            PrContentTab::Conversation => selected_entry,
            PrContentTab::Comments => selected_thread,
        };
        let scroll_key = (detail.number, tab, selection);
        if self.pull_request_scroll_key != Some(scroll_key) {
            let target = match tab {
                // Index 0 is the opening-post card; entries follow from there,
                // each preceded by a "pushed" card when commits landed first.
                PrContentTab::Conversation => selected_entry.map_or(0, |ix| {
                    ix + 1
                        + pr_pushed_commit_groups(&detail.commits, &detail.conversation)
                            .iter()
                            .take(ix + 1)
                            .filter(|group| !group.is_empty())
                            .count()
                }),
                // Index 0 is the filter-chips row; threads/headers follow.
                PrContentTab::Comments => selected_thread
                    .and_then(|ix| pr_thread_scroll_index(&threads, show_hidden, ix))
                    .map(|index| index + 1)
                    .unwrap_or(0),
            };
            self.pull_request_scroll.scroll_to_item(target);
            self.pull_request_scroll_key = Some(scroll_key);
        }

        let focus = self
            .history_view
            .read(cx)
            .history_panel_focus_handle
            .clone();
        let open_count = threads.ready().map_or(0, |threads| {
            threads
                .iter()
                .filter(|thread| !thread.is_resolved && !thread.outdated())
                .count()
        });
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let icon_size = ui_scale.px(14.0);
        let (kind, title) = crate::view::pr_symbols::title(&detail.title);
        let root_for_github = self.root_view.clone();
        let root_for_conversation = self.root_view.clone();
        let root_for_comments = self.root_view.clone();

        let conversation_active = tab == PrContentTab::Conversation;
        let mut conversation_tab = div()
            .id("pr_conversation_tab")
            .flex()
            .items_center()
            .h(ui_scale.px(24.0))
            .px_2()
            .rounded(px(theme.radii.pill))
            .cursor_pointer()
            .text_size(theme.ui_text(12.5))
            .child("Conversation");
        conversation_tab = if conversation_active {
            conversation_tab
                .bg(theme.colors.interaction.selected_background)
                .text_color(theme.colors.foreground.primary)
        } else {
            conversation_tab.text_color(theme.colors.foreground.secondary)
        };
        let conversation_tab = conversation_tab.on_activate(
            false,
            controls::ControlActivation::Composite,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                let focus = this
                    .history_view
                    .read(cx)
                    .history_panel_focus_handle
                    .clone();
                window.focus(&focus, cx);
                let root = root_for_conversation.clone();
                cx.defer(move |cx| {
                    let _ = root.update(cx, |root, cx| {
                        root.set_pull_request_content_tab(PrContentTab::Conversation, cx)
                    });
                });
            }),
        );

        let comments_active = tab == PrContentTab::Comments;
        let mut comments_tab = div()
            .id("pr_comments_tab")
            .flex()
            .items_center()
            .gap_1()
            .h(ui_scale.px(24.0))
            .px_2()
            .rounded(px(theme.radii.pill))
            .cursor_pointer()
            .text_size(theme.ui_text(12.5))
            .child("Comments")
            .child(
                div()
                    .flex_none()
                    .rounded(px(theme.radii.pill))
                    .bg(theme.colors.surface.raised)
                    .px_1()
                    .text_size(theme.ui_text(10.5))
                    .text_color(theme.colors.foreground.primary)
                    .child(format!("{open_count} open")),
            );
        comments_tab = if comments_active {
            comments_tab
                .bg(theme.colors.interaction.selected_background)
                .text_color(theme.colors.foreground.primary)
        } else {
            comments_tab.text_color(theme.colors.foreground.secondary)
        };
        let comments_tab = comments_tab.on_activate(
            false,
            controls::ControlActivation::Composite,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                let focus = this
                    .history_view
                    .read(cx)
                    .history_panel_focus_handle
                    .clone();
                window.focus(&focus, cx);
                let root = root_for_comments.clone();
                cx.defer(move |cx| {
                    let _ = root.update(cx, |root, cx| {
                        root.set_pull_request_content_tab(PrContentTab::Comments, cx)
                    });
                });
            }),
        );

        let header = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .h(ui_scale.px(38.0))
            .border_b_1()
            .border_color(theme.colors.stroke.subtle)
            .bg(theme.colors.surface.panel)
            .text_size(theme.ui_text(13.0))
            .child(
                crate::view::pr_symbols::state(&detail.state, detail.is_draft, theme).render(
                    format!("pr_header_{}_state", detail.number),
                    theme,
                    icon_size,
                ),
            )
            .when_some(kind, |header, kind| {
                header.child(crate::view::pr_symbols::kind_tag(
                    kind,
                    theme,
                    theme.ui_text(11.0),
                ))
            })
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.colors.foreground.secondary)
                    .child(format!("#{}", detail.number)),
            )
            .child(div().flex_none().w(px(8.0)))
            .child(conversation_tab)
            .child(comments_tab)
            .child(
                div()
                    .flex_none()
                    .ml_1()
                    .child(components::shortcut_keys("[+]", theme, ui_scale)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("pr_open_on_github")
                    .cursor_pointer()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(theme.ui_text(12.5))
                    .text_color(theme.colors.foreground.secondary)
                    .child("Open on GitHub")
                    .child(components::shortcut_keys("o", theme, ui_scale))
                    .on_activate(
                        false,
                        controls::ControlActivation::Composite,
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let focus = this
                                .history_view
                                .read(cx)
                                .history_panel_focus_handle
                                .clone();
                            window.focus(&focus, cx);
                            let root = root_for_github.clone();
                            cx.defer(move |cx| {
                                let _ = root
                                    .update(cx, |root, cx| root.open_pull_request_on_github(cx));
                            });
                        }),
                    ),
            );

        let body = match tab {
            PrContentTab::Conversation => {
                self.pr_conversation(&detail, &threads, selected_entry, cx)
            }
            PrContentTab::Comments => {
                self.pr_comments(number, &threads, selected_thread, show_hidden, cx)
            }
        };
        div()
            .size_full()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .track_focus(&focus)
            .child(header)
            .child(body)
            .into_any_element()
    }

    fn pr_conversation(
        &mut self,
        detail: &PullRequestDetail,
        threads: &PrLoad<Arc<Vec<ReviewThread>>>,
        selected_entry: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let mut body = div()
            .id("pr_content_scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .max_w(px(PR_CONTENT_MAX_WIDTH_PX))
            .gap_3()
            .px_3()
            .py_3()
            .overflow_y_scroll()
            .track_scroll(&self.pull_request_scroll);

        body = body.child(self.pr_description_card(detail, cx));

        let push_groups = pr_pushed_commit_groups(&detail.commits, &detail.conversation);
        for (ix, entry) in detail.conversation.iter().enumerate() {
            if !push_groups[ix].is_empty() {
                body = body.child(pr_pushed_event(&push_groups[ix], theme, ui_scale));
            }
            body = body.child(self.pr_conversation_entry(
                detail.number,
                ix,
                entry,
                threads,
                selected_entry,
                cx,
            ));
        }
        if let Some(last) = push_groups.last()
            && !last.is_empty()
        {
            body = body.child(pr_pushed_event(last, theme, ui_scale));
        }

        let checks_icon_size = ui_scale.px(13.0);
        body.child(
            div()
                .border_t_1()
                .border_color(theme.colors.stroke.subtle)
                .pt_2()
                .text_size(theme.ui_text(12.0))
                .child(
                    crate::view::pr_symbols::checks_line(
                        detail.checks,
                        &detail.head_oid,
                        theme,
                        checks_icon_size,
                    )
                    .unwrap_or_else(|| {
                        div()
                            .text_color(theme.colors.foreground.secondary)
                            .child("No checks")
                            .into_any_element()
                    }),
                ),
        )
        .into_any_element()
    }

    /// The opening post: the PR's own description, styled like every other
    /// Conversation card so it reads as the first entry.
    fn pr_description_card(
        &mut self,
        detail: &PullRequestDetail,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let avatar = components::author_avatar_sized(
            theme,
            ui_scale.px(MAIN_AVATAR_DIAMETER_PX),
            ui_scale.px(MAIN_AVATAR_FONT_PX),
            &detail.author,
        );
        let opened_label = if detail.created_at.is_empty() {
            "opened this pull request".to_owned()
        } else {
            format!(
                "opened this pull request · {}",
                pr_relative_time(&detail.created_at, std::time::SystemTime::now())
            )
        };
        let body_element = if detail.body.trim().is_empty() {
            div()
                .text_size(theme.ui_text(12.5))
                .text_color(theme.colors.foreground.secondary)
                .child("No description provided.")
                .into_any_element()
        } else {
            self.pr_markdown(detail.number, PrMarkdownKey::Body, &detail.body, cx)
        };
        div()
            .flex()
            .gap_3()
            .items_start()
            .child(avatar)
            .child(
                pr_card(theme, false)
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .bg(theme.colors.surface.panel)
                            .border_b_1()
                            .border_color(theme.colors.stroke.subtle)
                            .text_size(theme.ui_text(12.5))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(detail.author.clone()),
                            )
                            .child(
                                div()
                                    .text_color(theme.colors.foreground.secondary)
                                    .child(opened_label),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .flex_none()
                                    .rounded(px(theme.radii.pill))
                                    .border_1()
                                    .border_color(theme.colors.stroke.default)
                                    .px_2()
                                    .text_size(theme.ui_text(10.5))
                                    .text_color(theme.colors.foreground.secondary)
                                    .child("Description"),
                            ),
                    )
                    .child(div().px_4().py_3().child(body_element)),
            )
            .into_any_element()
    }

    fn pr_conversation_entry(
        &mut self,
        number: u64,
        ix: usize,
        entry: &ConversationEntry,
        threads: &PrLoad<Arc<Vec<ReviewThread>>>,
        selected: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let now = std::time::SystemTime::now();
        let when = pr_relative_time(&entry.at, now);
        let avatar = components::author_avatar_sized(
            theme,
            ui_scale.px(MAIN_AVATAR_DIAMETER_PX),
            ui_scale.px(MAIN_AVATAR_FONT_PX),
            &entry.author,
        );

        let mut header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .bg(theme.colors.surface.panel)
            .border_b_1()
            .border_color(theme.colors.stroke.subtle)
            .text_size(theme.ui_text(12.5))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(entry.author.clone()),
            );
        if entry.verb == "commented" {
            header = header.child(
                div()
                    .text_color(theme.colors.foreground.secondary)
                    .child(format!("commented {when}")),
            );
        } else {
            let status = match entry.verb {
                "approved" => PrReviewerStatus::Approved,
                "requested changes" => PrReviewerStatus::ChangesRequested,
                "reviewed" => PrReviewerStatus::Commented,
                "reviewed (dismissed)" => PrReviewerStatus::Dismissed,
                _ => PrReviewerStatus::Requested,
            };
            let status_result = crate::view::pr_symbols::reviewer_status(status, theme);
            let verb_color = status_result
                .as_ref()
                .ok()
                .map(|symbol| symbol.color())
                .unwrap_or(theme.colors.foreground.secondary);
            let status_icon = match status_result {
                Ok(symbol) => symbol.render(
                    format!("pr_conversation_entry_{ix}_status"),
                    theme,
                    ui_scale.px(12.0),
                ),
                Err(text) => div()
                    .text_color(theme.colors.foreground.secondary)
                    .child(text)
                    .into_any_element(),
            };
            header = header
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(verb_color)
                        .child(status_icon)
                        .child(entry.verb.to_string()),
                )
                .child(
                    div()
                        .text_color(theme.colors.foreground.secondary)
                        .child(when),
                );
        }

        let is_current = selected == Some(ix);
        let mut card = pr_card(theme, is_current)
            .flex_1()
            .min_w(px(0.0))
            .child(header);
        if !entry.body.is_empty() {
            card = card.child(div().px_4().py_3().child(self.pr_markdown(
                number,
                PrMarkdownKey::Entry(entry.id.clone()),
                &entry.body,
                cx,
            )));
        }
        // The review's own inline comments, matched by the REST review id
        // `view` filled in on `review_id` and each thread's own
        // `pull_request_review_id` — both from GitHub's REST API, so the same
        // numeric ids.
        if let Some(review_id) = entry.review_id
            && let Some(threads) = threads.ready()
        {
            let matching: Vec<&ReviewThread> = threads
                .iter()
                .filter(|thread| thread.pull_request_review_id == Some(review_id))
                .collect();
            if !matching.is_empty() {
                let count = matching.len();
                let mut paths: Vec<String> =
                    matching.iter().map(|thread| thread.path.clone()).collect();
                paths.sort();
                paths.dedup();
                card = card.child(div().px_4().pb_3().child(
                    crate::view::pr_symbols::review_line_comments_footer(
                        count,
                        &paths,
                        theme,
                        ui_scale.px(12.0),
                    ),
                ));
            }
        }

        let root = self.root_view.clone();
        div()
            .id(SharedString::from(format!("pr_conversation_entry_{ix}")))
            .flex()
            .gap_3()
            .items_start()
            .cursor_pointer()
            .child(avatar)
            .child(card)
            .on_activate(
                false,
                controls::ControlActivation::Composite,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let focus = this
                        .history_view
                        .read(cx)
                        .history_panel_focus_handle
                        .clone();
                    window.focus(&focus, cx);
                    let root = root.clone();
                    cx.defer(move |cx| {
                        let _ = root.update(cx, |root, cx| root.select_pull_request_entry(ix, cx));
                    });
                }),
            )
            .into_any_element()
    }

    fn pr_comments(
        &mut self,
        number: u64,
        threads: &PrLoad<Arc<Vec<ReviewThread>>>,
        selected: Option<usize>,
        show_hidden: bool,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let threads = match threads {
            PrLoad::Idle | PrLoad::Loading => {
                return components::empty_state_message(theme, "Loading review threads…")
                    .into_any_element();
            }
            PrLoad::Failed(err) => {
                return components::empty_state(
                    theme,
                    "Couldn't load review threads",
                    err.to_string(),
                )
                .into_any_element();
            }
            PrLoad::Ready(threads) => threads,
        };

        let (mut open_n, mut resolved_n, mut outdated_n) = (0usize, 0usize, 0usize);
        let mut path_open_counts: FxHashMap<&str, usize> = FxHashMap::default();
        for thread in threads.iter() {
            if thread.outdated() {
                outdated_n += 1;
            } else if thread.is_resolved {
                resolved_n += 1;
            } else {
                open_n += 1;
                *path_open_counts.entry(thread.path.as_str()).or_insert(0) += 1;
            }
        }

        let mut body = div()
            .id("pr_content_scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .max_w(px(PR_CONTENT_MAX_WIDTH_PX))
            .gap_2()
            .px_3()
            .py_3()
            .overflow_y_scroll()
            .track_scroll(&self.pull_request_scroll);

        // Informational counts, not independent filters: the view only ever
        // has two states (open-only, or `V` to show everything), so only the
        // "Open" chip can honestly claim to be selected.
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .pb_1()
                .child(crate::view::pr_symbols::filter_chip(
                    format!("Open {open_n}"),
                    !show_hidden,
                    theme,
                ))
                .child(crate::view::pr_symbols::filter_chip(
                    format!("Resolved {resolved_n}"),
                    false,
                    theme,
                ))
                .child(crate::view::pr_symbols::filter_chip(
                    format!("Outdated {outdated_n}"),
                    false,
                    theme,
                ))
                .child(div().flex_1())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(theme.ui_text(11.5))
                        .text_color(theme.colors.foreground.secondary)
                        .child(components::shortcut_keys("V", theme, ui_scale))
                        .child(if show_hidden {
                            "hide resolved and outdated"
                        } else {
                            "show resolved and outdated"
                        }),
                ),
        );

        let mut last_path: Option<&str> = None;
        let mut shown = 0;
        let icon_size = ui_scale.px(13.0);
        for ix in visible_pr_thread_indexes(threads, show_hidden) {
            let thread = &threads[ix];
            shown += 1;
            if last_path != Some(&thread.path) {
                last_path = Some(&thread.path);
                let open_here = path_open_counts
                    .get(thread.path.as_str())
                    .copied()
                    .unwrap_or(0);
                body = body.child(crate::view::pr_symbols::file_header(
                    &thread.path,
                    open_here,
                    theme,
                    icon_size,
                ));
            }
            let (status_label, status_set) = if thread.outdated() {
                ("Outdated", theme.colors.status.info)
            } else if thread.is_resolved {
                ("Resolved", theme.colors.status.success)
            } else {
                ("Open", theme.colors.status.warning)
            };
            let line_label = thread
                .line
                .or(thread.original_line)
                .map_or("File".to_owned(), |line| format!("Line {line}"));
            let is_current = selected == Some(ix);
            let row = if is_current {
                self.pr_thread_card(
                    number,
                    ix,
                    thread,
                    &line_label,
                    status_label,
                    status_set,
                    cx,
                )
            } else {
                self.pr_thread_row(ix, thread, &line_label, status_label, status_set, cx)
            };
            body = body.child(row);
        }
        let available = threads
            .iter()
            .filter(|thread| show_hidden || (!thread.is_resolved && !thread.outdated()))
            .count();
        if available > shown {
            body = body.child(
                div()
                    .text_size(theme.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child(format!(
                        "{} more threads not shown (showing first {MAX_PR_VISIBLE_THREADS})",
                        available - shown
                    )),
            );
        }
        if !show_hidden && resolved_n + outdated_n > 0 {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .text_size(theme.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child(format!(
                        "{resolved_n} resolved and {outdated_n} outdated thread{} hidden",
                        if resolved_n + outdated_n == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ))
                    .child(components::shortcut_keys("V", theme, ui_scale)),
            );
        }
        if shown == 0 {
            body = body.child(
                div()
                    .text_size(theme.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child(if threads.is_empty() {
                        "No review threads"
                    } else {
                        "No open threads · V shows resolved and outdated"
                    }),
            );
        }
        body.into_any_element()
    }

    /// A collapsed, single-row (~36px) review thread: everything but the
    /// keyboard-selected one, which expands into [`Self::pr_thread_card`].
    fn pr_thread_row(
        &mut self,
        ix: usize,
        thread: &ReviewThread,
        line_label: &str,
        status_label: &'static str,
        status_set: crate::theme::StatusColorSet,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let first = thread.comments.first();
        let avatar = first.map(|comment| {
            components::author_avatar_sized(
                theme,
                ui_scale.px(SMALL_AVATAR_DIAMETER_PX),
                ui_scale.px(SMALL_AVATAR_FONT_PX),
                &comment.author,
            )
        });
        let excerpt = first.map_or(String::new(), |comment| comment.body.clone());
        let reply_count = thread.comments.len().saturating_sub(1);

        let root = self.root_view.clone();
        div()
            .id(SharedString::from(format!("pr_thread_{ix}")))
            .flex()
            .items_center()
            .gap_2()
            .h(ui_scale.px(36.0))
            .px_2()
            .rounded(px(theme.radii.row))
            .border_1()
            .border_color(theme.colors.stroke.subtle)
            .cursor_pointer()
            .children(avatar)
            .child(
                div()
                    .flex_none()
                    .text_size(theme.ui_text(11.5))
                    .text_color(theme.colors.foreground.secondary)
                    .child(line_label.to_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(theme.ui_text(12.5))
                    .child(excerpt),
            )
            .child(crate::view::pr_symbols::bubble_count(
                reply_count,
                theme,
                ui_scale.px(12.0),
            ))
            .child(crate::view::pr_symbols::status_chip(
                status_label,
                status_set,
                theme,
            ))
            .on_activate(
                false,
                controls::ControlActivation::Composite,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let focus = this
                        .history_view
                        .read(cx)
                        .history_panel_focus_handle
                        .clone();
                    window.focus(&focus, cx);
                    let root = root.clone();
                    cx.defer(move |cx| {
                        let _ = root.update(cx, |root, cx| root.select_pull_request_thread(ix, cx));
                    });
                }),
            )
            .into_any_element()
    }

    /// The keyboard-selected review thread, expanded: its comments (cached
    /// markdown, matching Conversation entries) followed by its key hints.
    fn pr_thread_card(
        &mut self,
        number: u64,
        ix: usize,
        thread: &ReviewThread,
        line_label: &str,
        status_label: &'static str,
        status_set: crate::theme::StatusColorSet,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        let now = std::time::SystemTime::now();
        let gutter = ui_scale.px(40.0);
        let diff_context = diff_hunk_tail(&thread.diff_hunk, 3);
        let diff_strip = (!diff_context.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .py_1()
                .bg(theme.colors.surface.canvas)
                .border_b_1()
                .border_color(theme.colors.stroke.subtle)
                .children(diff_context.into_iter().map(|line| {
                    let (foreground, background) = match line.kind {
                        DiffHunkLineKind::Added => (
                            theme.colors.diff.added.foreground,
                            Some(theme.colors.diff.added.background),
                        ),
                        DiffHunkLineKind::Removed => (
                            theme.colors.diff.removed.foreground,
                            Some(theme.colors.diff.removed.background),
                        ),
                        DiffHunkLineKind::Context => (theme.colors.foreground.secondary, None),
                    };
                    div()
                        .flex()
                        .when_some(background, |row, background| row.bg(background))
                        .font_family(crate::font_preferences::EDITOR_MONOSPACE_FONT_FAMILY)
                        .text_size(theme.ui_text(12.0))
                        .child(
                            div()
                                .flex_none()
                                .w(gutter)
                                .px_1()
                                .text_color(theme.colors.foreground.secondary)
                                .child(
                                    line.line_number
                                        .map_or(String::new(), |number| number.to_string()),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_color(foreground)
                                .child(line.text),
                        )
                }))
        });

        let mut body_col = div().flex().flex_col().gap_3().px_3().py_3();
        for (comment_ix, comment) in thread.comments.iter().enumerate() {
            let avatar = components::author_avatar_sized(
                theme,
                ui_scale.px(SMALL_AVATAR_DIAMETER_PX),
                ui_scale.px(SMALL_AVATAR_FONT_PX),
                &comment.author,
            );
            let when = pr_relative_time(&comment.at, now);
            let key = if comment_ix == 0 {
                PrMarkdownKey::Thread(thread.root_id)
            } else {
                PrMarkdownKey::ThreadReply(thread.root_id, comment_ix)
            };

            let mut meta = div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(theme.ui_text(12.5))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(comment.author.clone()),
                );
            meta = if comment_ix == 0 {
                meta.child(
                    div()
                        .text_color(theme.colors.foreground.secondary)
                        .child(format!("{line_label} · {when}")),
                )
                .child(div().flex_1())
                .child(crate::view::pr_symbols::status_chip(
                    status_label,
                    status_set,
                    theme,
                ))
            } else {
                meta.child(
                    div()
                        .text_color(theme.colors.foreground.secondary)
                        .child(when),
                )
            };

            let mut row = div().flex().gap_2().items_start();
            if comment_ix > 0 {
                row = row.pl(px(24.0));
            }
            row = row.child(avatar).child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .gap_1()
                    .child(meta)
                    .child(self.pr_markdown(number, key, &comment.body, cx)),
            );
            body_col = body_col.child(row);
        }
        body_col = body_col.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .pt_1()
                .text_size(theme.ui_text(11.5))
                .text_color(theme.colors.foreground.secondary)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(components::shortcut_keys("enter", theme, ui_scale))
                        .child("go to the line"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(components::shortcut_keys("i+h", theme, ui_scale))
                        .child("thread helper"),
                ),
        );

        let root = self.root_view.clone();
        pr_card(theme, true)
            .children(diff_strip)
            .child(body_col)
            .id(SharedString::from(format!("pr_thread_{ix}")))
            .cursor_pointer()
            .on_activate(
                false,
                controls::ControlActivation::Composite,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let focus = this
                        .history_view
                        .read(cx)
                        .history_panel_focus_handle
                        .clone();
                    window.focus(&focus, cx);
                    let root = root.clone();
                    cx.defer(move |cx| {
                        let _ = root.update(cx, |root, cx| root.select_pull_request_thread(ix, cx));
                    });
                }),
            )
            .into_any_element()
    }

    fn pr_markdown(
        &mut self,
        number: u64,
        key: PrMarkdownKey,
        source: &str,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let Some(document) = self.pr_markdown_cache.document(number, key, source) else {
            return div()
                .text_size(self.theme.ui_text(12.0))
                .child(source.to_owned())
                .into_any_element();
        };
        let ui_scale = crate::ui_scale::UiScale::current(cx);
        rows::render_markdown_document(
            &document,
            &rows::MarkdownDocumentContext {
                theme: self.theme,
                ui_scale_percent: ui_scale.percent(),
                editor_font_family: crate::font_preferences::EDITOR_MONOSPACE_FONT_FAMILY.into(),
                image_root: None,
                remote_image_access: rows::MarkdownRemoteImageAccess {
                    policy: RemoteMarkdownImagePolicy::NeverLoad,
                    ..Default::default()
                },
                picture_sizes: Default::default(),
                drawn_pictures: None,
                row_boxes: Default::default(),
                block_scrolls: Default::default(),
                blocks: Default::default(),
                layout: Default::default(),
                view: None,
                text_region: DiffTextRegion::Inline,
                change_bar_color: None,
                query: None,
                reveal: Default::default(),
                scroll: None,
                hovered_link: None,
                change_extents: None,
                tasks_editable: false,
            },
        )
    }
}

/// "<login> pushed N commits · time", or "Pushed N commits · time" when the
/// group's first (oldest) commit has no author GitHub could report.
fn pr_pushed_headline(commits: &[&PullRequestCommit], when: &str) -> String {
    let count = commits.len();
    let plural = if count == 1 { "" } else { "s" };
    match commits
        .first()
        .and_then(|commit| commit.author.as_deref())
        .filter(|author| !author.is_empty())
    {
        Some(author) => format!("{author} pushed {count} commit{plural} · {when}"),
        None => format!("Pushed {count} commit{plural} · {when}"),
    }
}

/// A "Pushed N commits" timeline event: a commit-dot marker, when it
/// happened, and the commits themselves (short sha + headline) indented
/// underneath. One flattened child so it doesn't shift `scroll_to_item`
/// indices computed by [`MainPaneView::pull_request_view`].
fn pr_pushed_event(
    commits: &[&PullRequestCommit],
    theme: AppTheme,
    ui_scale: crate::ui_scale::UiScale,
) -> AnyElement {
    let now = std::time::SystemTime::now();
    let when = commits
        .last()
        .map(|commit| pr_relative_time(&commit.committed_at, now))
        .unwrap_or_default();
    let headline = pr_pushed_headline(commits, &when);
    let dot = ui_scale.px(6.0);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .pl_2()
                .text_size(theme.ui_text(12.0))
                .text_color(theme.colors.foreground.secondary)
                .child(
                    div()
                        .flex_none()
                        .w(dot)
                        .h(dot)
                        .rounded(dot * 0.5)
                        .bg(theme.colors.foreground.secondary),
                )
                .child(headline),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(px(28.0))
                .children(commits.iter().map(|commit| {
                    let short = commit.oid.get(..7).unwrap_or(&commit.oid);
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(theme.ui_text(12.0))
                        .child(
                            div()
                                .flex_none()
                                .font_family(crate::font_preferences::EDITOR_MONOSPACE_FONT_FAMILY)
                                .text_color(theme.colors.foreground.secondary)
                                .child(short.to_owned()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_color(theme.colors.foreground.primary)
                                .child(commit.headline.clone()),
                        )
                })),
        )
        .into_any_element()
}

/// GitHub prose never loads a remote picture. Keep its URL visible in the
/// Markdown layout; the read-only renderer deliberately has no link handlers.
fn pr_markdown_document(source: &str) -> Option<MarkdownPreviewDocument> {
    let mut document = crate::view::markdown_preview::parse_markdown(source)?;
    for row in &mut document.rows {
        if let Some(image) = row.image.as_ref()
            && let Some(url) = rows::markdown_preview_remote_image_url(image.source.as_ref())
        {
            let alt = row.text.as_ref();
            let prefix = if alt.is_empty() {
                "Image: ".to_owned()
            } else {
                format!("Image: {alt} — ")
            };
            let start = prefix.len();
            row.text = format!("{prefix}{url}").into();
            row.inline_spans = Arc::new(vec![MarkdownInlineSpan {
                byte_range: start..start + url.len(),
                style: MarkdownInlineStyle::Link,
                link_url: Some(url),
            }]);
            row.kind = MarkdownPreviewRowKind::Paragraph;
            row.image = None;
            row.styled_text_cache = Default::default();
        }

        let mut kept_images = Vec::with_capacity(row.inline_images.len());
        let mut text: Option<String> = None;
        let mut spans: Option<Vec<MarkdownInlineSpan>> = None;
        for inline in row.inline_images.iter() {
            if let Some(url) = rows::markdown_preview_remote_image_url(inline.image.source.as_ref())
            {
                let text = text.get_or_insert_with(|| row.text.to_string());
                let spans = spans.get_or_insert_with(|| row.inline_spans.as_ref().clone());
                if !text.is_empty() {
                    text.push(' ');
                }
                if inline.alt.is_empty() {
                    text.push_str("Image: ");
                } else {
                    text.push_str(&format!("Image: {} — ", inline.alt));
                }
                let start = text.len();
                text.push_str(&url);
                spans.push(MarkdownInlineSpan {
                    byte_range: start..text.len(),
                    style: MarkdownInlineStyle::Link,
                    link_url: Some(url),
                });
            } else {
                kept_images.push(inline.clone());
            }
        }
        if let (Some(text), Some(spans)) = (text, spans) {
            row.text = text.into();
            row.inline_spans = Arc::new(spans);
            row.inline_images = Arc::from(kept_images);
            row.styled_text_cache = Default::default();
        }
    }
    Some(document)
}

fn pr_thread_scroll_index(
    threads: &PrLoad<Arc<Vec<ReviewThread>>>,
    show_hidden: bool,
    selected: usize,
) -> Option<usize> {
    let mut index = 0;
    let mut last_path: Option<&str> = None;
    let threads = threads.ready()?;
    for ix in visible_pr_thread_indexes(threads, show_hidden) {
        let thread = &threads[ix];
        if last_path != Some(thread.path.as_str()) {
            index += 1;
            last_path = Some(&thread.path);
        }
        if ix == selected {
            return Some(index);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_are_grouped_in_conversation_gaps() {
        let commits = ["1", "2", "3", "5"].map(|at| PullRequestCommit {
            oid: at.into(),
            headline: String::new(),
            committed_at: at.into(),
            author: None,
        });
        let entries = ["2", "4"].map(|at| ConversationEntry {
            id: at.into(),
            author: String::new(),
            verb: "commented",
            at: at.into(),
            body: String::new(),
            review_id: None,
        });
        let groups = pr_pushed_commit_groups(&commits, &entries);
        let group_oids: Vec<Vec<&str>> = groups
            .iter()
            .map(|group| group.iter().map(|commit| commit.oid.as_str()).collect())
            .collect();
        assert_eq!(group_oids, [vec!["1", "2"], vec!["3"], vec!["5"]]);
    }

    #[test]
    fn remote_markdown_images_become_url_links() {
        let document = pr_markdown_document(
            "![plot](https://example.com/plot.png)\n\nText ![icon](https://example.com/icon.png)",
        )
        .expect("Markdown parses");
        assert!(document.rows.iter().all(|row| row.image.is_none()));
        assert!(document.rows.iter().all(|row| row.inline_images.is_empty()));
        for url in [
            "https://example.com/plot.png",
            "https://example.com/icon.png",
        ] {
            assert!(document.rows.iter().any(|row| {
                row.inline_spans.iter().any(|span| {
                    span.link_url
                        .as_ref()
                        .is_some_and(|link| link.as_ref() == url)
                        && &row.text[span.byte_range.clone()] == url
                })
            }));
        }
    }

    #[test]
    fn pr_markdown_cache_reuses_documents_only_for_the_same_pr_item_and_source() {
        let mut cache = PrMarkdownCache::default();
        let key = PrMarkdownKey::Entry("comment-1".into());
        let first = cache.document(7, key.clone(), "# First").unwrap();
        let again = cache.document(7, key.clone(), "# First").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let edited = cache.document(7, key.clone(), "# Edited").unwrap();
        assert!(!Arc::ptr_eq(&first, &edited));
        let other_pr = cache.document(8, key, "# Edited").unwrap();
        assert!(!Arc::ptr_eq(&edited, &other_pr));
    }

    #[test]
    fn diff_hunk_tail_derives_line_numbers_by_walking_from_the_header() {
        let hunk = "@@ -10,3 +10,4 @@ fn thing\n context\n-old\n+new one\n+new two";
        let tail = diff_hunk_tail(hunk, 3);
        assert_eq!(
            tail.iter()
                .map(|line| (line.kind, line.line_number, line.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (DiffHunkLineKind::Removed, Some(11), "old"),
                (DiffHunkLineKind::Added, Some(11), "new one"),
                (DiffHunkLineKind::Added, Some(12), "new two"),
            ]
        );
    }

    #[test]
    fn diff_hunk_tail_skips_the_no_newline_marker_and_caps_line_length() {
        let long_line = "x".repeat(MAX_HUNK_LINE_CHARS + 50);
        let hunk = format!("@@ -1,1 +1,1 @@\n+{long_line}\n\\ No newline at end of file");
        let tail = diff_hunk_tail(&hunk, 3);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].text.chars().count(), MAX_HUNK_LINE_CHARS);
    }

    #[test]
    fn diff_hunk_tail_is_empty_without_a_parsable_header() {
        assert!(diff_hunk_tail("", 3).is_empty());
        assert!(diff_hunk_tail("not a hunk", 3).is_empty());
    }

    #[test]
    fn pushed_headline_names_the_first_commits_author_and_falls_back_without_one() {
        let named = PullRequestCommit {
            oid: "a".repeat(40),
            headline: "First".into(),
            committed_at: "2026-01-01T00:00:00Z".into(),
            author: Some("alexk".into()),
        };
        let anonymous = PullRequestCommit {
            oid: "b".repeat(40),
            headline: "Second".into(),
            committed_at: "2026-01-01T00:00:01Z".into(),
            author: None,
        };
        assert_eq!(
            pr_pushed_headline(&[&named, &anonymous], "5 hours ago"),
            "alexk pushed 2 commits · 5 hours ago"
        );
        // The *first* (oldest) commit's author names the push, not the last's.
        assert_eq!(
            pr_pushed_headline(&[&anonymous, &named], "5 hours ago"),
            "Pushed 2 commits · 5 hours ago"
        );
        assert_eq!(
            pr_pushed_headline(&[&anonymous], "just now"),
            "Pushed 1 commit · just now"
        );
    }
}
