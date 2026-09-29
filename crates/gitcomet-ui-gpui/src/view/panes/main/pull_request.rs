use super::*;
use crate::github::{
    ConversationEntry, PrReviewerStatus, PullRequestCommit, PullRequestDetail, ReviewThread,
};
use crate::kit::interaction::{self as controls, ControlInteractionExt as _};
use crate::view::RemoteMarkdownImagePolicy;
use crate::view::markdown_preview::{
    MarkdownInlineSpan, MarkdownInlineStyle, MarkdownPreviewDocument, MarkdownPreviewRowKind,
};
use crate::view::pull_requests::{
    MAX_PR_VISIBLE_THREADS, PrContentTab, PrLoad, visible_pr_thread_indexes,
};
use rustc_hash::FxHashMap;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(in crate::view) enum PrMarkdownKey {
    Body,
    Entry(String),
    Thread(u64),
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

/// One synthetic push row for each conversation gap containing commits.
fn pr_push_counts(commits: &[PullRequestCommit], entries: &[ConversationEntry]) -> Vec<usize> {
    let mut dates: Vec<&str> = commits
        .iter()
        .map(|commit| commit.committed_at.as_str())
        .collect();
    dates.sort_unstable();
    let mut next = 0;
    let mut counts = Vec::with_capacity(entries.len() + 1);
    for entry in entries {
        let start = next;
        while next < dates.len() && dates[next] <= entry.at.as_str() {
            next += 1;
        }
        counts.push(next - start);
    }
    counts.push(dates.len() - next);
    counts
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
                PrContentTab::Conversation => selected_entry.map_or(0, |ix| {
                    ix + 2
                        + pr_push_counts(&detail.commits, &detail.conversation)
                            .iter()
                            .take(ix + 1)
                            .filter(|count| **count > 0)
                            .count()
                }),
                PrContentTab::Comments => selected_thread
                    .and_then(|ix| pr_thread_scroll_index(&threads, show_hidden, ix))
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
        let icon_size = crate::ui_scale::UiScale::current(cx).px(14.0);
        let (kind, title) = crate::view::pr_symbols::title(&detail.title);
        let root_for_github = self.root_view.clone();
        let root_for_conversation = self.root_view.clone();
        let root_for_comments = self.root_view.clone();
        let header = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.colors.stroke.subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(theme.ui_text(14.0))
                    .child(
                        crate::view::pr_symbols::state(&detail.state, detail.is_draft, theme)
                            .render(
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
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .child(title.to_owned()),
                    )
                    .child(
                        div()
                            .text_color(theme.colors.foreground.secondary)
                            .child(format!("#{}", detail.number)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_size(theme.ui_text(12.0))
                    .child(
                        div()
                            .id("pr_conversation_tab")
                            .cursor_pointer()
                            .text_color(if tab == PrContentTab::Conversation {
                                theme.colors.foreground.primary
                            } else {
                                theme.colors.foreground.secondary
                            })
                            .child("Conversation")
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
                                    let root = root_for_conversation.clone();
                                    cx.defer(move |cx| {
                                        let _ = root.update(cx, |root, cx| {
                                            root.set_pull_request_content_tab(
                                                PrContentTab::Conversation,
                                                cx,
                                            )
                                        });
                                    });
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("pr_comments_tab")
                            .cursor_pointer()
                            .text_color(if tab == PrContentTab::Comments {
                                theme.colors.foreground.primary
                            } else {
                                theme.colors.foreground.secondary
                            })
                            .child(format!("Comments ({open_count})"))
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
                                    let root = root_for_comments.clone();
                                    cx.defer(move |cx| {
                                        let _ = root.update(cx, |root, cx| {
                                            root.set_pull_request_content_tab(
                                                PrContentTab::Comments,
                                                cx,
                                            )
                                        });
                                    });
                                }),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("pr_open_on_github")
                            .cursor_pointer()
                            .text_color(theme.colors.foreground.secondary)
                            .child("Open on GitHub  o")
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
                                        let _ = root.update(cx, |root, cx| {
                                            root.open_pull_request_on_github(cx)
                                        });
                                    });
                                }),
                            ),
                    ),
            );

        let body = match tab {
            PrContentTab::Conversation => self.pr_conversation(&detail, selected_entry, cx),
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
        selected_entry: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let mut body = div()
            .id("pr_content_scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap_3()
            .px_3()
            .py_3()
            .overflow_y_scroll()
            .track_scroll(&self.pull_request_scroll);
        body = body.child(
            div()
                .text_size(theme.ui_text(11.0))
                .text_color(theme.colors.foreground.secondary)
                .child(format!("{} opened this pull request", detail.author)),
        );
        body = body.child(if detail.body.trim().is_empty() {
            div()
                .text_size(theme.ui_text(12.0))
                .text_color(theme.colors.foreground.secondary)
                .child("No description")
                .into_any_element()
        } else {
            self.pr_markdown(detail.number, PrMarkdownKey::Body, &detail.body, cx)
        });
        // GitHub gives commit times rather than push times. Group commits by
        // the conversation gap their commit time falls in.
        let push_counts = pr_push_counts(&detail.commits, &detail.conversation);
        let pushed_event = |count: usize| {
            div()
                .px_2()
                .py_2()
                .text_size(theme.ui_text(12.0))
                .text_color(theme.colors.foreground.secondary)
                .child(format!(
                    "Pushed {} commit{}",
                    count,
                    if count == 1 { "" } else { "s" }
                ))
        };
        for (ix, entry) in detail.conversation.iter().enumerate() {
            if push_counts[ix] > 0 {
                body = body.child(pushed_event(push_counts[ix]));
            }
            body = body.child(self.pr_conversation_entry(
                detail.number,
                ix,
                entry,
                selected_entry,
                cx,
            ));
        }
        if push_counts[detail.conversation.len()] > 0 {
            body = body.child(pushed_event(push_counts[detail.conversation.len()]));
        }
        let checks = detail.checks;
        body.child(
            div()
                .border_t_1()
                .border_color(theme.colors.stroke.subtle)
                .pt_2()
                .text_size(theme.ui_text(12.0))
                .child(if checks.total() == 0 {
                    "No checks".to_owned()
                } else {
                    format!(
                        "Checks: {} passing · {} failing · {} pending",
                        checks.passing, checks.failing, checks.pending
                    )
                }),
        )
        .into_any_element()
    }

    fn pr_conversation_entry(
        &mut self,
        number: u64,
        ix: usize,
        entry: &ConversationEntry,
        selected: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let status = match entry.verb {
            "approved" => PrReviewerStatus::Approved,
            "requested changes" => PrReviewerStatus::ChangesRequested,
            "commented" | "reviewed" => PrReviewerStatus::Commented,
            "reviewed (dismissed)" => PrReviewerStatus::Dismissed,
            _ => PrReviewerStatus::Requested,
        };
        let icon_size = crate::ui_scale::UiScale::current(cx).px(12.0);
        let status_icon = match crate::view::pr_symbols::reviewer_status(status, theme) {
            Ok(symbol) => symbol.render(
                format!("pr_conversation_entry_{ix}_status"),
                theme,
                icon_size,
            ),
            Err(text) => div()
                .text_color(theme.colors.foreground.secondary)
                .child(text)
                .into_any_element(),
        };
        let mut row = div()
            .id(SharedString::from(format!("pr_conversation_entry_{ix}")))
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_2()
            .rounded(px(theme.radii.control))
            .bg(if selected == Some(ix) {
                theme.colors.interaction.selected_background
            } else {
                theme.colors.surface.panel
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(theme.ui_text(12.0))
                    .child(status_icon)
                    .child(format!(
                        "{} {} · {}",
                        entry.author,
                        entry.verb,
                        entry.at.get(..10).unwrap_or(&entry.at)
                    )),
            );
        if !entry.body.is_empty() {
            row = row.child(self.pr_markdown(
                number,
                PrMarkdownKey::Entry(entry.id.clone()),
                &entry.body,
                cx,
            ));
        }
        let root = self.root_view.clone();
        row.cursor_pointer()
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
        let mut body = div()
            .id("pr_content_scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap_2()
            .px_3()
            .py_3()
            .overflow_y_scroll()
            .track_scroll(&self.pull_request_scroll);
        let mut last_path: Option<&str> = None;
        let mut shown = 0;
        for ix in visible_pr_thread_indexes(threads, show_hidden) {
            let thread = &threads[ix];
            shown += 1;
            if last_path != Some(&thread.path) {
                last_path = Some(&thread.path);
                body = body.child(
                    div()
                        .pt_2()
                        .text_size(theme.ui_text(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(thread.path.clone()),
                );
            }
            let status = if thread.outdated() {
                "Outdated"
            } else if thread.is_resolved {
                "Resolved"
            } else {
                "Open"
            };
            let first = thread.comments.first();
            let line = thread
                .line
                .or(thread.original_line)
                .map_or("File".to_owned(), |line| format!("Line {line}"));
            let mut row = div()
                .id(SharedString::from(format!("pr_thread_{ix}")))
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_2()
                .rounded(px(theme.radii.control))
                .bg(if selected == Some(ix) {
                    theme.colors.interaction.selected_background
                } else {
                    theme.colors.surface.panel
                })
                .child(
                    div()
                        .text_size(theme.ui_text(11.0))
                        .text_color(theme.colors.foreground.secondary)
                        .child(format!(
                            "{line} · {status} · {} {}",
                            thread.comments.len().saturating_sub(1),
                            if thread.comments.len() == 2 {
                                "reply"
                            } else {
                                "replies"
                            }
                        )),
                );
            if let Some(first) = first {
                row = row.child(
                    div()
                        .text_size(theme.ui_text(12.0))
                        .child(first.author.clone()),
                );
                row = row.child(self.pr_markdown(
                    number,
                    PrMarkdownKey::Thread(thread.root_id),
                    &first.body,
                    cx,
                ));
            }
            let root = self.root_view.clone();
            body = body.child(row.cursor_pointer().on_activate(
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
            ));
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
        });
        let entries = ["2", "4"].map(|at| ConversationEntry {
            id: at.into(),
            author: String::new(),
            verb: "commented",
            at: at.into(),
            body: String::new(),
        });
        assert_eq!(pr_push_counts(&commits, &entries), [2, 1, 1]);
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
}
